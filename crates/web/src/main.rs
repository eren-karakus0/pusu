//! PUSU web arayüzü giriş noktası. Tüm mantık modüllerde; burada yalnızca
//! panik kancasını kurup uygulamayı body'ye mount ediyoruz.

mod alert;
mod api;
mod app;
mod bulk;
mod config;
mod onboarding;
mod wallet;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}
