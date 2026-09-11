//! Legacy shell global shortcut registration and live replacement.

use super::*;

impl Processor {
    /// Create the global hotkey manager and register the quick-terminal toggle
    /// (Ctrl+`). Returns `(None, None)` if the platform rejects it, so the rest
    /// of the terminal keeps working without a quick terminal.
    pub(super) fn init_quick_hotkey(combo: &str) -> (Option<GlobalHotKeyManager>, Option<HotKey>) {
        if combo.trim().is_empty() {
            return (None, None);
        }
        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => manager,
            Err(err) => {
                warn!("Quick terminal disabled: global hotkey init failed: {err}");
                return (None, None);
            },
        };
        let hotkey = combo
            .parse::<HotKey>()
            .unwrap_or_else(|_| HotKey::new(Some(HotKeyModifiers::CONTROL), Code::Backquote));
        match manager.register(hotkey) {
            Ok(()) => (Some(manager), Some(hotkey)),
            Err(err) => {
                // Non-fatal and common in dev: a hard-killed previous instance
                // never ran Drop to release Ctrl+`, or another app already owns
                // it. The terminal works fine without the quick-terminal hotkey,
                // so log quietly instead of nagging the on-screen message bar.
                debug!("Quick terminal hotkey (Ctrl+`) not registered: {err}");
                (Some(manager), None)
            },
        }
    }

    /// Drain global-hotkey events and toggle the quick terminal on a press.
    pub(super) fn poll_quick_hotkey(&mut self, event_loop: &ActiveEventLoop) {
        let Some(hotkey_id) = self.quick_hotkey.map(|hotkey| hotkey.id()) else { return };
        let mut toggle = false;
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.id == hotkey_id && event.state == HotKeyState::Pressed {
                toggle = true;
            }
        }
        if toggle {
            self.toggle_quick_terminal(event_loop);
        }
    }

    /// Replace the global quick-terminal shortcut transactionally. Registering
    /// the candidate before releasing the old key keeps the existing shortcut
    /// alive when the OS rejects a conflicting or malformed candidate.
    pub(super) fn apply_quick_terminal_hotkey(&mut self, requested: &str) -> Result<(), String> {
        if requested.trim().is_empty() {
            if let (Some(manager), Some(hotkey)) = (&self.global_hotkey, self.quick_hotkey) {
                manager.unregister(hotkey).map_err(|error| error.to_string())?;
            }
            self.quick_hotkey = None;
            self.quick_hotkey_combo.clear();
            return Ok(());
        }
        let new_hotkey =
            requested.parse::<HotKey>().map_err(|err| format!("快捷键格式无效：{err}"))?;
        if self.quick_hotkey == Some(new_hotkey) {
            self.quick_hotkey_combo = requested.to_owned();
            return Ok(());
        }

        let mut manager = self.global_hotkey.take().or_else(|| GlobalHotKeyManager::new().ok());
        let Some(manager_ref) = manager.as_mut() else {
            return Err("系统全局快捷键管理器初始化失败".to_owned());
        };
        if let Err(err) = manager_ref.register(new_hotkey) {
            self.global_hotkey = manager;
            return Err(format!("快捷键注册失败：{err}"));
        }
        if let Some(old_hotkey) = self.quick_hotkey {
            if let Err(err) = manager_ref.unregister(old_hotkey) {
                let _ = manager_ref.unregister(new_hotkey);
                self.global_hotkey = manager;
                return Err(format!("释放旧快捷键失败：{err}"));
            }
        }
        self.global_hotkey = manager;
        self.quick_hotkey = Some(new_hotkey);
        self.quick_hotkey_combo = requested.to_owned();
        Ok(())
    }

    /// Settings files are shared by windows. A non-originating window can
    /// notice a changed hotkey during its mtime reload, so drain one staged
    /// request during the main wait cycle even when no key event follows.
    pub(super) fn flush_quick_hotkey_requests(&mut self) {
        let mut pending = None;
        for (window_id, window_context) in &mut self.windows {
            if let Some(hotkey) = window_context.display.take_quick_hotkey_request() {
                pending = Some((*window_id, hotkey));
                break;
            }
        }
        let Some((window_id, hotkey)) = pending else { return };
        let old = self.quick_hotkey_combo.clone();
        let result = self.apply_quick_terminal_hotkey(&hotkey);
        if let Some(window_context) = self.windows.get_mut(&window_id) {
            match result {
                Ok(()) => {
                    window_context.display.quick_hotkey_registration_done(&hotkey, true, None, &old)
                },
                Err(err) => window_context.display.quick_hotkey_registration_done(
                    &hotkey,
                    false,
                    Some(&err),
                    &old,
                ),
            }
            window_context.dirty = true;
            window_context.display.window.request_redraw();
        }
    }
}
