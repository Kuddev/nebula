//! Markdown and tooltip adapter for the shared SMILES depiction engine.

use crate::i18n::Message;
use gpui::prelude::*;
use gpui::{App, Context, IntoElement, ObjectFit, Render, SharedString, Window, div, img, px};
use gpui_component::ActiveTheme;

pub(crate) struct MoleculeView {
    source: SharedString,
}

impl MoleculeView {
    pub(crate) fn new(source: SharedString, _: &mut Context<Self>) -> Self {
        Self { source }
    }
}

impl Render for MoleculeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let result = super::scientific_render::assets(cx)
            .molecule(self.source.clone(), cx.theme().is_dark());
        let loading = result.is_none();
        let invalid = matches!(result, Some(None));
        let image = result.flatten();
        let language = super::config::ui_language(cx);
        div()
            .w_full()
            .min_w_0()
            .max_w(px(600.0))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("SMILES"))
            .when_some(image, |root, image| {
                root.child(img(image).w_full().h(px(240.0)).object_fit(ObjectFit::Contain))
            })
            .when(loading, |root| {
                root.child(div().h(px(240.0)).child(language.text(Message::ChemistryLoading)))
            })
            .when(invalid, |root| {
                root.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(language.text(Message::ChemistryInvalid)),
                )
            })
            .child(
                div()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .whitespace_normal()
                    .child(self.source.clone()),
            )
    }
}

pub(super) fn markdown_extensions() -> gpui_component::text::MarkdownExtensions {
    use gpui_component::text::{MarkdownExtensions, MarkdownNode};
    if !crate::chemistry::STRUCTURE_RENDERING_ENABLED {
        // No custom parser: SMILES fences remain ordinary, copyable code blocks.
        return MarkdownExtensions::default();
    }
    MarkdownExtensions::default()
        .block_parser(|node, _| {
            let markdown::mdast::Node::Code(code) = node else { return None };
            if !code.lang.as_deref().is_some_and(|lang| lang.eq_ignore_ascii_case("smiles")) {
                return None;
            }
            Some(
                MarkdownNode::new("pebrel-smiles", code.value.clone())
                    .text(code.value.clone())
                    .markdown(format!("```smiles\n{}\n```", code.value)),
            )
        })
        .block_renderer("pebrel-smiles", |node, window, cx: &mut App| {
            let source: SharedString = node.data::<String>().cloned().unwrap_or_default().into();
            let state = window.use_keyed_state(source.clone(), cx, |_, cx| {
                cx.new(|cx| MoleculeView::new(source, cx))
            });
            state.read(cx).clone()
        })
}
