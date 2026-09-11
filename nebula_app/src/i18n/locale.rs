// Native discovery stays in the platform adapter. The independent translation
// contract compiles this same adapter without the application shell.
#[path = "../platform/locale.rs"]
mod platform_locale;

pub use platform_locale::system_locale;
