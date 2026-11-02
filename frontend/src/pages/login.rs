use crate::{api::ApiClient, state::AuthState};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::UserProfile;

#[component]
pub fn LoginPage(on_success: Callback<()>) -> impl IntoView {
    let auth = expect_context::<AuthState>();

    let email = RwSignal::new(String::from("superadmin@pyraocean.local"));
    let password = RwSignal::new(String::new());
    let error_msg = RwSignal::new(Option::<String>::None);
    let is_loading = RwSignal::new(false);

    let submit_login = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let email_val = email.get().trim().to_string();
        let pass_val = password.get();

        if email_val.is_empty() || pass_val.is_empty() {
            error_msg.set(Some("Please enter both your email and password.".into()));
            return;
        }

        is_loading.set(true);
        error_msg.set(None);

        spawn_local(async move {
            match ApiClient::login(email_val, pass_val).await {
                Ok(resp) => {
                    auth.set_session(resp.token, UserProfile::from(resp.user));
                    is_loading.set(false);
                    on_success.run(());
                }
                Err(err) => {
                    is_loading.set(false);
                    error_msg.set(Some(err));
                }
            }
        });
    };

    view! {
        <div
            class=move || {
                if auth.dark_mode.get() {
                    "min-h-screen bg-[#14171A] text-[#F1F5F9] flex flex-col justify-between p-6 transition-colors"
                } else {
                    "min-h-screen bg-[#F8FAFC] text-[#0F172A] flex flex-col justify-between p-6 transition-colors"
                }
            }
        >
            // Top bar with PyraOcean Logo & Theme Switcher
            <header class="w-full max-w-6xl mx-auto flex items-center justify-between py-3">
                <div class="flex items-center gap-3">
                    <img
                        src=move || auth.active_logo_url()
                        alt="PyraOcean"
                        class="h-10 w-10 rounded-lg object-contain border border-[#06B6D4]/40 shadow-sm"
                    />
                    <div>
                        <span class="text-lg font-bold tracking-tight">
                            "Pyra"<span class="text-[#06B6D4]">"Ocean"</span>
                        </span>
                        <p class="text-xs text-slate-400">"Self-Hosted Dynamic Grid & Workspace Platform"</p>
                    </div>
                </div>

                <button
                    type="button"
                    on:click=move |_| auth.toggle_theme()
                    class="px-3 py-1.5 rounded-lg border border-[#2A2F37] text-xs font-medium hover:border-[#06B6D4] transition"
                >
                    {move || if auth.dark_mode.get() { "☀ Light Mode" } else { "☾ Dark Mode" }}
                </button>
            </header>

            // Center Login Card
            <section
                class=move || {
                    let card_cls = if auth.dark_mode.get() {
                        "bg-[#1E2227] border-[#2A2F37] text-[#F1F5F9]"
                    } else {
                        "bg-white border-[#E2E8F0] text-[#0F172A]"
                    };
                    format!("w-full max-w-md mx-auto my-8 border rounded-2xl p-8 shadow-2xl {card_cls}")
                }
            >
                <div class="flex flex-col items-center text-center mb-6">
                    <img
                        src=move || auth.active_logo_url()
                        alt="PyraOcean Logo"
                        class="h-20 w-20 rounded-2xl object-contain mb-4 border border-[#06B6D4]/30 shadow-md"
                    />
                    <h1 class="text-2xl font-bold tracking-tight">
                        "Sign in to Pyra"<span class="text-[#06B6D4]">"Ocean"</span>
                    </h1>
                    <p class="text-xs text-slate-400 mt-1">
                        "Enter your enterprise credentials to access your Spaces and Dynamic Grids"
                    </p>
                </div>

                {move || {
                    error_msg.get().map(|msg| {
                        view! {
                            <div class="mb-4 p-3 rounded-lg bg-red-500/15 border border-red-500/40 text-xs text-red-400">
                                {msg}
                            </div>
                        }
                    })
                }}

                <form class="space-y-4" on:submit=submit_login>
                    <div>
                        <label class="block text-xs font-semibold uppercase tracking-wider text-slate-400 mb-1.5">
                            "Email Address"
                        </label>
                        <input
                            type="email"
                            required
                            placeholder="superadmin@pyraocean.local"
                            class="w-full px-3.5 py-2.5 rounded-lg bg-[#14171A]/90 border border-[#2A2F37] text-[#F1F5F9] text-sm focus:outline-none focus:border-[#06B6D4]"
                            prop:value=email
                            on:input=move |ev| email.set(event_target_value(&ev))
                        />
                    </div>

                    <div>
                        <label class="block text-xs font-semibold uppercase tracking-wider text-slate-400 mb-1.5">
                            "Password"
                        </label>
                        <input
                            type="password"
                            required
                            placeholder="••••••••••••"
                            class="w-full px-3.5 py-2.5 rounded-lg bg-[#14171A]/90 border border-[#2A2F37] text-[#F1F5F9] text-sm focus:outline-none focus:border-[#06B6D4]"
                            prop:value=password
                            on:input=move |ev| password.set(event_target_value(&ev))
                        />
                    </div>

                    <button
                        type="submit"
                        disabled=move || is_loading.get()
                        class="w-full py-2.5 px-4 rounded-lg bg-gradient-to-r from-[#F97316] to-[#EA580C] hover:from-[#EA580C] hover:to-[#F97316] text-white font-semibold text-sm shadow-lg transition disabled:opacity-50"
                    >
                        {move || if is_loading.get() { "Authenticating..." } else { "Sign In to Workspace" }}
                    </button>
                </form>

                // Powered by Pyramakerz Attribution inside Login Card
                <div class="mt-8 pt-5 border-t border-[#2A2F37]/60 flex items-center justify-center gap-2.5">
                    <img
                        src="/public/pyramakerz.webp"
                        alt="Pyramakerz"
                        class="h-6 w-6 object-contain"
                    />
                    <span class="text-xs font-medium text-slate-400">
                        "Powered by "
                        <span class="text-[#F97316] font-semibold">"Pyramakerz"</span>
                    </span>
                </div>
            </section>

            // Bottom Footer
            <footer class="w-full max-w-6xl mx-auto text-center text-xs text-slate-500 py-2 flex items-center justify-center gap-2">
                <img src="/public/pyramakerz.webp" alt="Pyramakerz" class="h-4 w-4 object-contain" />
                <span>"PyraOcean Enterprise Edition — Powered by Pyramakerz"</span>
            </footer>
        </div>
    }
}

