//! Exercise the real Select popup, including its virtual list and search.

use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;
use gpui_component::select::SearchableVec;

#[derive(Clone)]
struct ProbeItem(ShellSelectItem);

impl SelectItem for ProbeItem {
    type Value = String;

    fn title(&self) -> SharedString {
        self.0.title()
    }

    fn value(&self) -> &String {
        self.0.value()
    }

    fn display_title(&self) -> Option<gpui::AnyElement> {
        self.0.display_title()
    }

    fn matches(&self, query: &str) -> bool {
        self.0.matches(query)
    }

    fn render(&self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let selector = format!("shell-row-{}", self.0.id);
        div().debug_selector(move || selector.clone()).child(self.0.render(window, cx))
    }
}

struct ShellPickerProbe {
    select: Entity<SelectState<SearchableVec<ProbeItem>>>,
}

impl Render for ShellPickerProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_4().child(
            div()
                .debug_selector(|| "shell-picker-trigger".to_owned())
                .w(px(240.0))
                .h(px(32.0))
                .child(Select::new(&self.select)),
        )
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx.debug_bounds(selector).expect("visible picker control").center();
    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
    draw(cx);
}

#[gpui::test]
fn shell_popup_keeps_mixed_rows_separate_before_and_after_search(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let mut select = None;
    let (_, mut window) = cx.add_window_view(|window, cx| {
        let items = vec![
            ShellSelectItem::import_action(crate::display::UiLanguage::ZhCn),
            ShellSelectItem::new("pwsh".into(), "PowerShell 7".into(), 1.0),
            ShellSelectItem::new("powershell".into(), "PowerShell".into(), 1.0),
            ShellSelectItem::new(
                "wsl:Ubuntu".into(),
                "WSL · Ubuntu · 长名称仍保持单行且不覆盖相邻选项".into(),
                1.0,
            ),
            ShellSelectItem::new("zsh".into(), "Zsh".into(), 1.0),
        ]
        .into_iter()
        .map(ProbeItem)
        .collect::<Vec<_>>();
        let state = cx.new(|cx| {
            // Exercise the optional search mode with its actual filtering
            // delegate; a plain Vec deliberately leaves the rows unfiltered.
            SelectState::new(SearchableVec::new(items), Some(IndexPath::new(1)), window, cx)
                .searchable(true)
        });
        select = Some(state.clone());
        let view = cx.new(|_| ShellPickerProbe { select: state });
        Root::new(view, window, cx)
    });
    draw(&mut window);
    click("shell-picker-trigger", &mut window);

    let selectors = [
        "shell-row-__nebula_import_terminal_dir__",
        "shell-row-pwsh",
        "shell-row-powershell",
        "shell-row-wsl:Ubuntu",
        "shell-row-zsh",
    ];
    let bounds: Vec<_> = selectors
        .iter()
        .map(|selector| window.debug_bounds(selector).expect("real popup row"))
        .collect();
    for adjacent in bounds.windows(2) {
        assert!(adjacent[0].origin.y + adjacent[0].size.height <= adjacent[1].origin.y);
    }
    for row in &bounds {
        assert_eq!(row.size.height, bounds[1].size.height);
    }

    window.simulate_input("PowerShell");
    draw(&mut window);
    let first = window.debug_bounds("shell-row-pwsh").expect("filtered PowerShell 7 row");
    let second = window.debug_bounds("shell-row-powershell").expect("filtered PowerShell row");
    assert!(first.origin.y + first.size.height <= second.origin.y);
    assert_eq!(second.origin.y - first.origin.y, bounds[2].origin.y - bounds[1].origin.y);
    assert!(window.debug_bounds("shell-row-wsl:Ubuntu").is_none());
    click("shell-row-powershell", &mut window);
    window.update(|_, cx| {
        assert_eq!(
            select.as_ref().unwrap().read(cx).selected_value().map(String::as_str),
            Some("powershell")
        );
    });
}
