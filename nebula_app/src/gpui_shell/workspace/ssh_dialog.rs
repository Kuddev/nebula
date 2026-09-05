use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, AppContext as _, ParentElement as _, Styled as _, Window, div};

use crate::gpui_shell::prelude::*;
use crate::ssh_prompt::{Prompt, PromptKind, PromptResponse};

pub(super) fn show(request: Arc<Prompt>, window: &mut Window, cx: &mut App) {
    if !request.is_pending() {
        return;
    }
    let language = crate::gpui_shell::config::ui_language(cx);
    let (title, description, is_secret, allow_save) = match &request.kind {
        PromptKind::HostKey { host, port, fingerprint } => (
            language.pick("验证 SSH 主机", "Verify SSH host"),
            format!("{host}:{port}\n\n{fingerprint}\n\n{}", language.pick(
                "请通过可信渠道核对指纹。仅在确认主机身份后信任并保存。",
                "Verify this fingerprint through a trusted channel before trusting and saving it.",
            )),
            false,
            false,
        ),
        PromptKind::Secret { label, allow_save } => {
            (language.pick("SSH 身份验证", "SSH authentication"), label.clone(), true, *allow_save)
        },
    };
    let input = cx.new(|cx| InputState::new(window, cx).masked(true));
    let remember = Rc::new(Cell::new(false));
    let focus_input = input.clone();
    window.open_dialog(cx, move |dialog, window, _cx| {
        let submitted = request.clone();
        let closed = request.clone();
        let submit_input = input.clone();
        let clear_input = input.clone();
        let save = remember.clone();
        let toggle_save = remember.clone();
        let mut body = div().w_full().flex().flex_col().gap_3();
        if is_secret {
            body = body.child(Input::new(&input).mask_toggle().w_full());
        }
        if allow_save {
            body = body.child(
                Checkbox::new("ssh-remember-secret")
                    .label(language.pick("保存在系统凭据库", "Save in system credential store"))
                    .checked(remember.get())
                    .on_click(move |value, window, _| {
                        toggle_save.set(*value);
                        window.refresh();
                    }),
            );
        }
        confirm_dialog(
            dialog,
            window,
            title,
            description.clone(),
            if is_secret {
                language.pick("继续", "Continue")
            } else {
                language.pick("信任并连接", "Trust and connect")
            },
            language.pick("取消", "Cancel"),
            ButtonVariant::Primary,
        )
        .child(body)
        .on_ok(move |_, window, cx| {
            let response = if is_secret {
                PromptResponse::Secret {
                    value: zeroize::Zeroizing::new(
                        submit_input.read(cx).value().as_bytes().to_vec(),
                    ),
                    save: allow_save && save.get(),
                }
            } else {
                PromptResponse::Trust
            };
            submitted.respond(response);
            submit_input.update(cx, |input, cx| input.set_value("", window, cx));
            true
        })
        .on_close(move |_, window, cx| {
            closed.respond(PromptResponse::Cancel);
            clear_input.update(cx, |input, cx| input.set_value("", window, cx));
        })
    });
    if is_secret {
        focus_input.update(cx, |input, cx| input.focus(window, cx));
    }
}

#[cfg(all(test, feature = "gpui-test-support"))]
mod tests {
    use super::*;
    use gpui::{Context, IntoElement, Modifiers, Render, TestAppContext, point};
    use gpui_component::Root;

    struct DialogProbe;

    impl Render for DialogProbe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
            div().size_full().children(Root::render_dialog_layer(window, cx))
        }
    }

    fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
        let bounds = cx.debug_bounds(selector).expect("SSH dialog button is visible");
        cx.simulate_click(
            point(
                bounds.origin.x + bounds.size.width * 0.5,
                bounds.origin.y + bounds.size.height * 0.5,
            ),
            Modifiers::default(),
        );
    }

    #[gpui::test]
    fn ssh_dialog_cancel_does_not_accept_an_unknown_host(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| DialogProbe);
            Root::new(view, window, cx)
        });
        let (request, mut response) = Prompt::for_test(PromptKind::HostKey {
            host: "example.test".into(),
            port: 22,
            fingerprint: "SHA256:verify-me".into(),
        });
        cx.update(|window, cx| {
            show(request, window, cx);
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        click(cx, "confirm-dialog-cancel");
        assert_eq!(
            response.try_recv().map(|response| matches!(response, PromptResponse::Cancel)),
            Ok(true)
        );
    }

    #[gpui::test]
    fn ssh_dialog_password_is_delivered_only_on_confirmation(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| DialogProbe);
            Root::new(view, window, cx)
        });
        let (request, mut response) = Prompt::for_test(PromptKind::Secret {
            label: "Password for example.test".into(),
            allow_save: true,
        });
        cx.update(|window, cx| {
            show(request, window, cx);
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_input("test-secret");
        assert!(response.try_recv().is_err());
        click(cx, "confirm-dialog-ok");
        match response.try_recv() {
            Ok(PromptResponse::Secret { value, save }) => {
                assert_eq!(&*value, b"test-secret");
                assert!(!save);
            },
            _ => panic!("confirm must deliver the entered password"),
        }
    }
}
