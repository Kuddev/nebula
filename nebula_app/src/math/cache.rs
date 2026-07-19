//! 固定字节预算的公式布局 LRU。

use std::collections::HashMap;
use std::mem::size_of;

use ahash::RandomState;

use super::layout::{MathGlyphOp, MathLayout, MathRuleOp, MathTextOp};
use super::{MathError, MathErrorKind};

pub(crate) const LAYOUT_CACHE_BUDGET: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FormulaCacheKey {
    /// 文档生命周期内稳定的公式编号；缓存随 DocView 一起销毁，因而无需复制公式源码。
    pub(crate) formula_id: u64,
    pub(crate) pixel_size_bits: u32,
    pub(crate) pixels_per_point_bits: u32,
    pub(crate) display: bool,
}

impl FormulaCacheKey {
    pub(crate) fn new(
        formula_id: u64,
        pixel_size: f32,
        pixels_per_point: f32,
        display: bool,
    ) -> Self {
        Self {
            formula_id,
            pixel_size_bits: pixel_size.to_bits(),
            pixels_per_point_bits: pixels_per_point.to_bits(),
            display,
        }
    }
}

struct CacheEntry {
    key: FormulaCacheKey,
    layout: MathLayout,
    charge: usize,
}

#[derive(Default)]
struct CacheNode {
    entry: Option<CacheEntry>,
    previous: Option<usize>,
    next: Option<usize>,
    next_free: Option<usize>,
}

pub(crate) struct MathLayoutCache {
    budget: usize,
    used: usize,
    entries: HashMap<FormulaCacheKey, usize, RandomState>,
    nodes: Vec<CacheNode>,
    most_recent: Option<usize>,
    least_recent: Option<usize>,
    free: Option<usize>,
}

impl Default for MathLayoutCache {
    fn default() -> Self {
        Self::with_budget(LAYOUT_CACHE_BUDGET)
    }
}

impl MathLayoutCache {
    fn with_budget(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            entries: HashMap::with_hasher(RandomState::default()),
            nodes: Vec::new(),
            most_recent: None,
            least_recent: None,
            free: None,
        }
    }

    pub(crate) fn get(&mut self, key: FormulaCacheKey) -> Option<&MathLayout> {
        let index = *self.entries.get(&key)?;
        self.touch(index);
        self.nodes[index].entry.as_ref().map(|entry| &entry.layout)
    }

    pub(crate) fn get_or_insert_with(
        &mut self,
        key: FormulaCacheKey,
        build: impl FnOnce() -> Result<MathLayout, MathError>,
    ) -> Result<&MathLayout, MathError> {
        if let Some(index) = self.entries.get(&key).copied() {
            self.touch(index);
            return self.nodes[index]
                .entry
                .as_ref()
                .map(|entry| &entry.layout)
                .ok_or_else(|| MathError::new(MathErrorKind::Parse, 0));
        }

        let layout = build()?;
        let charge = layout_charge(&layout);
        if charge > self.budget {
            return Err(MathError::new(MathErrorKind::OpLimit, 0));
        }
        while self.used.saturating_add(charge) > self.budget {
            self.evict_least_recent()?;
        }

        let index = if let Some(index) = self.free {
            self.free = self.nodes[index].next_free.take();
            index
        } else {
            self.nodes.push(CacheNode::default());
            self.nodes.len() - 1
        };
        self.nodes[index].entry = Some(CacheEntry { key, layout, charge });
        self.used += charge;
        self.entries.insert(key, index);
        self.attach_most_recent(index);
        Ok(&self.nodes[index].entry.as_ref().expect("inserted cache node").layout)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn used_bytes(&self) -> usize {
        self.used
    }

    fn touch(&mut self, index: usize) {
        if self.most_recent == Some(index) {
            return;
        }
        self.detach(index);
        self.attach_most_recent(index);
    }

    fn detach(&mut self, index: usize) {
        let previous = self.nodes[index].previous.take();
        let next = self.nodes[index].next.take();
        if let Some(previous) = previous {
            self.nodes[previous].next = next;
        } else if self.most_recent == Some(index) {
            self.most_recent = next;
        }
        if let Some(next) = next {
            self.nodes[next].previous = previous;
        } else if self.least_recent == Some(index) {
            self.least_recent = previous;
        }
    }

    fn attach_most_recent(&mut self, index: usize) {
        let old_head = self.most_recent;
        self.nodes[index].previous = None;
        self.nodes[index].next = old_head;
        if let Some(old_head) = old_head {
            self.nodes[old_head].previous = Some(index);
        } else {
            self.least_recent = Some(index);
        }
        self.most_recent = Some(index);
    }

    fn evict_least_recent(&mut self) -> Result<(), MathError> {
        let index = self.least_recent.ok_or_else(|| MathError::new(MathErrorKind::OpLimit, 0))?;
        self.detach(index);
        let entry = self.nodes[index]
            .entry
            .take()
            .ok_or_else(|| MathError::new(MathErrorKind::Parse, 0))?;
        self.entries.remove(&entry.key);
        self.used = self.used.saturating_sub(entry.charge);
        self.nodes[index].next_free = self.free;
        self.free = Some(index);
        Ok(())
    }
}

fn layout_charge(layout: &MathLayout) -> usize {
    // 将容器、索引节点与哈希桶的近似固定开销也计入预算，避免大量空公式绕过限制。
    const INDEX_OVERHEAD: usize = size_of::<CacheNode>()
        + size_of::<(FormulaCacheKey, usize)>() * 2
        + size_of::<CacheEntry>();
    INDEX_OVERHEAD
        .saturating_add(layout.glyphs.capacity() * size_of::<MathGlyphOp>())
        .saturating_add(layout.rules.capacity() * size_of::<MathRuleOp>())
        .saturating_add(layout.text.capacity() * size_of::<MathTextOp>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::layout::{MathGlyphOp, MathMetrics};

    fn key(id: u64) -> FormulaCacheKey {
        FormulaCacheKey::new(id, 18.0, 1.0, false)
    }

    fn layout(glyphs: usize) -> MathLayout {
        MathLayout {
            metrics: MathMetrics::default(),
            glyphs: vec![MathGlyphOp::default(); glyphs],
            rules: Vec::new(),
            text: Vec::new(),
        }
    }

    #[test]
    fn lru_stays_within_its_byte_budget_and_reuses_nodes() {
        let one_charge = layout_charge(&layout(8));
        let mut cache = MathLayoutCache::with_budget(one_charge * 2);
        cache.get_or_insert_with(key(1), || Ok(layout(8))).unwrap();
        cache.get_or_insert_with(key(2), || Ok(layout(8))).unwrap();
        assert!(cache.get(key(1)).is_some());
        cache.get_or_insert_with(key(3), || Ok(layout(8))).unwrap();

        assert_eq!(cache.len(), 2);
        assert!(cache.get(key(1)).is_some());
        assert!(cache.get(key(2)).is_none());
        assert!(cache.get(key(3)).is_some());
        assert!(cache.used_bytes() <= one_charge * 2);
        assert_eq!(cache.nodes.len(), 2);
    }

    #[test]
    fn cache_hit_does_not_rebuild_layout() {
        let mut cache = MathLayoutCache::default();
        cache.get_or_insert_with(key(7), || Ok(layout(1))).unwrap();
        let hit =
            cache.get_or_insert_with(key(7), || panic!("cache hit must not call builder")).unwrap();
        assert_eq!(hit.glyphs.len(), 1);
    }
}
