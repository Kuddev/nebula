//! Application-scoped scientific assets. Only bounded background jobs compile
//! formulas, rasterize glyphs or depict molecules; views retain source identities.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::{StreamExt as _, channel::mpsc};
use gpui::{App, BackgroundExecutor, RenderImage, Rgba, SharedString, SvgRenderer};

use super::math_view::{ImageGeometry, compose_image};
use crate::math::layout::MathLayout;
use crate::math::{DEFAULT_LIMITS, compile_formula, compile_formula_source};
use crate::render_cache::RenderCache;

const CACHE_BYTES: usize = 48 * 1024 * 1024;
const CACHE_ENTRIES: usize = 1024;
const MAX_PENDING: usize = 64;
const PENDING_BYTES: usize = 4 * 1024 * 1024;
const MAX_RUNNING: usize = 2;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct FormulaKey {
    source: SharedString,
    display: bool,
    verbatim: bool,
    size: u32,
    points: u32,
}

impl FormulaKey {
    pub(super) fn new(
        source: SharedString,
        display: bool,
        verbatim: bool,
        size: f32,
        points: f32,
    ) -> Self {
        Self { source, display, verbatim, size: size.to_bits(), points: points.to_bits() }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Key {
    Layout(FormulaKey),
    Image(FormulaKey, u32, [u32; 4]),
    Molecule(SharedString, bool),
}

impl Key {
    fn bytes(&self) -> usize {
        256 + match self {
            Self::Layout(formula) | Self::Image(formula, ..) => formula.source.len(),
            Self::Molecule(source, _) => source.len(),
        }
    }
}

#[derive(Clone)]
enum Resource {
    Layout(Arc<MathLayout>),
    Image(Arc<RenderImage>, ImageGeometry),
    Molecule(Arc<RenderImage>),
    Failed,
}

impl Resource {
    fn bytes(&self) -> usize {
        match self {
            Self::Layout(layout) => layout_bytes(layout),
            // SVG rasterization may supersample. Charge actual pixel storage,
            // never logical SVG dimensions or assumed scale factors.
            Self::Image(image, _) | Self::Molecule(image) => {
                image.as_bytes(0).map_or(0, |bytes| bytes.len())
            },
            Self::Failed => 1,
        }
    }
}

fn layout_bytes(layout: &MathLayout) -> usize {
    layout.allocated_bytes()
}

struct Job {
    key: Key,
    layout: Option<Arc<MathLayout>>,
    charge: usize,
    bitmap_reservation: usize,
}

struct State {
    cache: RenderCache<Key, Resource>,
    pending: HashSet<Key>,
    queue: VecDeque<Job>,
    pending_bytes: usize,
    running: usize,
    reserved_bitmap_bytes: usize,
}

impl Default for State {
    fn default() -> Self {
        Self {
            cache: RenderCache::new(CACHE_BYTES, CACHE_ENTRIES),
            pending: HashSet::new(),
            queue: VecDeque::new(),
            pending_bytes: 0,
            running: 0,
            reserved_bitmap_bytes: 0,
        }
    }
}

impl State {
    fn admit(&mut self, job: Job) -> bool {
        if self.pending.contains(&job.key)
            || self.pending.len() >= MAX_PENDING
            || self.pending_bytes.saturating_add(job.charge) > PENDING_BYTES
            || job.bitmap_reservation > CACHE_BYTES
        {
            return false;
        }
        self.pending_bytes += job.charge;
        self.pending.insert(job.key.clone());
        self.queue.push_back(job);
        true
    }

    fn start(&mut self) -> Option<Job> {
        if self.running >= MAX_RUNNING {
            return None;
        }
        let reservation = self.queue.front()?.bitmap_reservation;
        if self.reserved_bitmap_bytes.saturating_add(reservation) > CACHE_BYTES {
            return None;
        }
        self.reserved_bitmap_bytes += reservation;
        // Evict before the worker allocates, not after its image already exists.
        self.cache.set_byte_budget(CACHE_BYTES - self.reserved_bitmap_bytes);
        let job = self.queue.pop_front()?;
        self.running += 1;
        Some(job)
    }

    fn finish(&mut self, job: &Job, result: Resource) {
        self.running -= 1;
        self.reserved_bitmap_bytes -= job.bitmap_reservation;
        self.cache.set_byte_budget(CACHE_BYTES - self.reserved_bitmap_bytes);
        self.pending_bytes -= job.charge;
        self.pending.remove(&job.key);
        let charge = job.key.bytes().saturating_add(result.bytes());
        self.cache.insert(job.key.clone(), result, charge);
    }
}

pub(super) struct ScientificRender {
    state: Mutex<State>,
    executor: BackgroundExecutor,
    svg: SvgRenderer,
    changed: mpsc::UnboundedSender<()>,
    wake_pending: AtomicBool,
    document_jobs: AtomicUsize,
}

struct Assets(Arc<ScientificRender>);
impl gpui::Global for Assets {}

pub(super) fn init(cx: &mut App) {
    let (changed, mut receiver) = mpsc::unbounded();
    let renderer = Arc::new(ScientificRender {
        state: Mutex::new(State::default()),
        executor: cx.background_executor().clone(),
        svg: cx.svg_renderer(),
        changed,
        wake_pending: AtomicBool::new(false),
        document_jobs: AtomicUsize::new(0),
    });
    cx.set_global(Assets(renderer.clone()));
    cx.spawn(async move |cx| {
        while receiver.next().await.is_some() {
            renderer.wake_pending.store(false, Ordering::Release);
            cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
}

pub(super) fn assets(cx: &App) -> Arc<ScientificRender> {
    cx.global::<Assets>().0.clone()
}

pub(super) struct DocumentPermit(Arc<ScientificRender>);
impl Drop for DocumentPermit {
    fn drop(&mut self) {
        self.0.document_jobs.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ScientificRender {
    pub(super) async fn document_permit(self: &Arc<Self>) -> DocumentPermit {
        loop {
            if self
                .document_jobs
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    (count < 2).then_some(count + 1)
                })
                .is_ok()
            {
                return DocumentPermit(self.clone());
            }
            self.executor.timer(std::time::Duration::from_millis(10)).await;
        }
    }

    fn request(self: &Arc<Self>, key: Key, layout: Option<Arc<MathLayout>>) -> Option<Resource> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(resource) = state.cache.get(&key) {
            return Some(resource.clone());
        }
        let charge = key.bytes() + layout.as_deref().map_or(0, layout_bytes);
        let bitmap_reservation = match (&key, layout.as_deref()) {
            (Key::Image(_, scale, _), Some(layout)) => {
                crate::math::bitmap::required_bytes(layout, f32::from_bits(*scale))
                    .map_or(0, |bytes| bytes.saturating_add(key.bytes()))
            },
            _ => 0,
        };
        let admitted = state.admit(Job { key, layout, charge, bitmap_reservation });
        drop(state);
        if admitted {
            self.drive();
        }
        None
    }

    fn drive(self: &Arc<Self>) {
        loop {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if self.changed.is_closed() {
                state.queue.clear();
                return;
            }
            let Some(job) = state.start() else { return };
            drop(state);
            let engine = self.clone();
            self.executor
                .spawn(async move {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        engine.build(&job)
                    }))
                    .unwrap_or(Resource::Failed);
                    engine.state.lock().unwrap_or_else(|e| e.into_inner()).finish(&job, result);
                    if !engine.wake_pending.swap(true, Ordering::AcqRel) {
                        let _ = engine.changed.unbounded_send(());
                    }
                    engine.drive();
                })
                .detach();
        }
    }

    fn build(&self, job: &Job) -> Resource {
        match &job.key {
            Key::Layout(key) => {
                let compile = if key.verbatim { compile_formula_source } else { compile_formula };
                compile(
                    &key.source,
                    key.display,
                    f32::from_bits(key.size),
                    f32::from_bits(key.points),
                    DEFAULT_LIMITS,
                )
                .map(|layout| Resource::Layout(Arc::new(layout)))
                .unwrap_or(Resource::Failed)
            },
            Key::Image(_, scale, color) => {
                let Some(layout) = &job.layout else { return Resource::Failed };
                let Ok(rasterizer) = crate::math::rasterizer::MathGlyphRasterizer::new() else {
                    return Resource::Failed;
                };
                let color = Rgba {
                    r: f32::from_bits(color[0]),
                    g: f32::from_bits(color[1]),
                    b: f32::from_bits(color[2]),
                    a: f32::from_bits(color[3]),
                };
                compose_image(&rasterizer, layout, f32::from_bits(*scale), color)
                    .map(|(image, geometry)| Resource::Image(image, geometry))
                    .unwrap_or(Resource::Failed)
            },
            Key::Molecule(source, dark) => crate::chemistry::render_smiles(source, *dark)
                .ok()
                .and_then(|svg| self.svg.render_single_frame(svg.as_bytes(), 1.0).ok())
                .map(Resource::Molecule)
                .unwrap_or(Resource::Failed),
        }
    }

    pub(super) fn layout(self: &Arc<Self>, key: FormulaKey) -> Option<Arc<MathLayout>> {
        match self.request(Key::Layout(key), None)? {
            Resource::Layout(layout) => Some(layout),
            _ => None,
        }
    }

    pub(super) fn image(
        self: &Arc<Self>,
        key: FormulaKey,
        scale: f32,
        color: Rgba,
    ) -> Option<(Arc<RenderImage>, ImageGeometry)> {
        let image_key = Key::Image(
            key.clone(),
            scale.to_bits(),
            [color.r.to_bits(), color.g.to_bits(), color.b.to_bits(), color.a.to_bits()],
        );
        // A hit never needs to look up or rebuild the corresponding layout.
        if let Some(resource) =
            self.state.lock().unwrap_or_else(|e| e.into_inner()).cache.get(&image_key).cloned()
        {
            return match resource {
                Resource::Image(image, geometry) => Some((image, geometry)),
                _ => None,
            };
        }
        let layout = self.layout(key)?;
        match self.request(image_key, Some(layout))? {
            Resource::Image(image, geometry) => Some((image, geometry)),
            _ => None,
        }
    }

    /// Outer None is pending; Some(None) is a cached source-text fallback.
    pub(super) fn molecule(
        self: &Arc<Self>,
        source: SharedString,
        dark: bool,
    ) -> Option<Option<Arc<RenderImage>>> {
        if !crate::chemistry::STRUCTURE_RENDERING_ENABLED {
            return Some(None);
        }
        match self.request(Key::Molecule(source, dark), None)? {
            Resource::Molecule(image) => Some(Some(image)),
            _ => Some(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: usize) -> Job {
        Job {
            key: Key::Molecule(format!("C{id}").into(), false),
            layout: None,
            charge: 1024,
            bitmap_reservation: 0,
        }
    }

    #[test]
    fn bitmap_workers_reserve_before_start_and_release_on_failure() {
        let mut state = State::default();
        state.cache.insert(job(99).key, Resource::Failed, CACHE_BYTES);
        for id in 0..2 {
            let mut work = job(id);
            work.bitmap_reservation = CACHE_BYTES / 2 + 1;
            assert!(state.admit(work));
        }
        let first = state.start().unwrap();
        assert!(state.cache.used_bytes() + state.reserved_bitmap_bytes <= CACHE_BYTES);
        assert_eq!(state.cache.len(), 0, "old image must be released before allocation");
        assert!(state.start().is_none(), "worker count alone cannot bound image memory");
        state.finish(&first, Resource::Failed);
        assert_eq!(state.reserved_bitmap_bytes, 0);
        let second = state.start().unwrap();
        assert!(state.cache.used_bytes() + state.reserved_bitmap_bytes <= CACHE_BYTES);
        state.finish(&second, Resource::Failed);
        assert_eq!(state.reserved_bitmap_bytes, 0);
        assert_eq!(state.pending_bytes, 0);
    }

    #[test]
    fn eighty_session_floods_have_bounded_work_without_duplicate_jobs() {
        let mut state = State::default();
        for _ in 0..20 {
            for session in 0..80 {
                state.admit(job(session));
            }
        }
        assert_eq!(state.pending.len(), MAX_PENDING);
        assert_eq!(state.queue.len(), MAX_PENDING);
        assert_eq!(state.pending_bytes, MAX_PENDING * 1024);
        let first = state.start().unwrap();
        let second = state.start().unwrap();
        assert!(state.start().is_none());
        assert!(!state.admit(job(0)));
        state.finish(&first, Resource::Failed);
        assert!(state.start().is_some());
        state.finish(&second, Resource::Failed);
        assert!(state.running <= MAX_RUNNING);
    }

    #[test]
    fn queued_layout_bytes_are_bounded_even_when_entry_count_is_small() {
        let mut state = State::default();
        let mut large = job(0);
        large.charge = PENDING_BYTES + 1;
        assert!(!state.admit(large));
        assert_eq!(state.pending_bytes, 0);
        assert!(state.queue.is_empty());
    }
}

#[cfg(test)]
mod corpus_tests;
