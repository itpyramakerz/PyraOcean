use crate::{api::ApiClient, state::AuthState};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::UserProfile;

#[component]
pub fn LoginPage(on_success: Callback<()>) -> impl IntoView {
    let auth = expect_context::<AuthState>();

    let email = RwSignal::new(String::from("superadmin@pyraocean.local"));
    let password = RwSignal::new(String::new());
    let totp_code = RwSignal::new(String::new());
    let requires_totp_step = RwSignal::new(false);

    // Self-Service Forgot Password Recovery state
    let show_forgot_password = RwSignal::new(false);
    let recovery_code_input = RwSignal::new(String::new());
    let reset_new_password = RwSignal::new(String::new());
    let info_msg = RwSignal::new(Option::<String>::None);

    let error_msg = RwSignal::new(Option::<String>::None);
    let is_loading = RwSignal::new(false);

    // Google OAuth2 Public Config & Login state
    let google_oauth_enabled = RwSignal::new(false);
    let google_client_id = RwSignal::new(String::new());
    let show_google_modal = RwSignal::new(false);
    let google_credential_input = RwSignal::new(String::new());
    let google_2fa_pending = RwSignal::new(false);
    let google_temp_2fa_token = RwSignal::new(Option::<String>::None);

    // Fetch `/api/auth/public-config` on load
    Effect::new(move |_| {
        spawn_local(async move {
            if let Ok(cfg) = ApiClient::get_public_auth_config().await {
                google_oauth_enabled.set(cfg.google_oauth_enabled);
                google_client_id.set(cfg.google_client_id);
            }
        });
    });

    let submit_google_login = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let email_val = email.get().trim().to_string();
        let cred_val = {
            let raw = google_credential_input.get().trim().to_string();
            if raw.is_empty() {
                email_val.clone()
            } else {
                raw
            }
        };
        let totp_opt = {
            let raw = totp_code.get().trim().to_string();
            if raw.is_empty() {
                None
            } else {
                Some(raw)
            }
        };

        if cred_val.is_empty() {
            error_msg.set(Some(
                "Please enter your Google Workspace email or Google ID Token.".into(),
            ));
            return;
        }

        is_loading.set(true);
        error_msg.set(None);
        info_msg.set(None);

        let temp_tok = google_temp_2fa_token.get_untracked();
        spawn_local(async move {
            match ApiClient::google_login(cred_val, Some(email_val), totp_opt, temp_tok).await {
                Ok(resp) => {
                    is_loading.set(false);
                    show_google_modal.set(false);
                    if resp.requires_totp || resp.requires_2fa {
                        requires_totp_step.set(true);
                        google_2fa_pending.set(true);
                        google_temp_2fa_token.set(resp.temp_2fa_token);
                        info_msg.set(Some(
                            "Google account verified! Please enter your 6-digit Google Authenticator 2FA code below to complete sign-in."
                                .into(),
                        ));
                    } else {
                        google_2fa_pending.set(false);
                        auth.set_session(resp.token, UserProfile::from(resp.user));
                        on_success.run(());
                    }
                }
                Err(err) => {
                    is_loading.set(false);
                    error_msg.set(Some(err));
                }
            }
        });
    };

    let submit_login = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        if google_2fa_pending.get_untracked() {
            submit_google_login(ev);
            return;
        }
        let email_val = email.get().trim().to_string();
        let pass_val = password.get();
        let totp_opt = {
            let raw = totp_code.get().trim().to_string();
            if raw.is_empty() {
                None
            } else {
                Some(raw)
            }
        };

        if email_val.is_empty() || pass_val.is_empty() {
            error_msg.set(Some("Please enter both your email and password.".into()));
            return;
        }

        if requires_totp_step.get_untracked() && totp_opt.is_none() {
            error_msg.set(Some(
                "Please enter your 6-digit 2FA Authenticator code or Recovery Code.".into(),
            ));
            return;
        }

        is_loading.set(true);
        error_msg.set(None);
        info_msg.set(None);

        spawn_local(async move {
            match ApiClient::login(email_val, pass_val, totp_opt).await {
                Ok(resp) => {
                    is_loading.set(false);
                    if resp.requires_totp {
                        requires_totp_step.set(true);
                        info_msg.set(Some(
                            "Two-Factor Authentication (2FA) is enabled for this account. Enter your 6-digit Authenticator code or Recovery Code below.".into(),
                        ));
                    } else {
                        auth.set_session(resp.token, UserProfile::from(resp.user));
                        on_success.run(());
                    }
                }
                Err(err) => {
                    is_loading.set(false);
                    error_msg.set(Some(err));
                }
            }
        });
    };

    // Handler for "Notify SuperAdmin for Password Reset" (`POST /api/auth/forgot-password/request`)
    let handle_notify_admin_reset = move |_| {
        let email_val = email.get().trim().to_string();
        if email_val.is_empty() || !email_val.contains('@') {
            error_msg.set(Some("Please enter your work email address first.".into()));
            return;
        }
        is_loading.set(true);
        error_msg.set(None);
        info_msg.set(None);

        spawn_local(async move {
            match ApiClient::request_password_reset(email_val).await {
                Ok(resp) => {
                    is_loading.set(false);
                    info_msg.set(Some(resp.message));
                }
                Err(e) => {
                    is_loading.set(false);
                    error_msg.set(Some(e));
                }
            }
        });
    };

    // Handler for Self-Service Reset via 2FA TOTP Code or Recovery Code (`POST /api/auth/forgot-password/reset`)
    let submit_self_service_reset = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let email_val = email.get().trim().to_string();
        let code_val = recovery_code_input.get().trim().to_string();
        let next_pass = reset_new_password.get();

        if email_val.is_empty() || code_val.is_empty() || next_pass.len() < 8 {
            error_msg.set(Some(
                "Please provide your email, 6-digit 2FA / Recovery Code, and a new password (min 8 chars).".into(),
            ));
            return;
        }

        is_loading.set(true);
        error_msg.set(None);
        info_msg.set(None);

        spawn_local(async move {
            match ApiClient::self_service_reset_password(email_val, code_val, next_pass).await {
                Ok(msg) => {
                    is_loading.set(false);
                    show_forgot_password.set(false);
                    recovery_code_input.set(String::new());
                    reset_new_password.set(String::new());
                    password.set(String::new());
                    info_msg.set(Some(msg));
                }
                Err(e) => {
                    is_loading.set(false);
                    error_msg.set(Some(e));
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
                        <span class="text-lg font-bold tracking-tight text-[#06B6D4]">
                            "PyraOcean"
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

            // Center Login / 2FA / Forgot Password Card
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
                        {move || {
                            if show_forgot_password.get() {
                                view! { <span>"Account "<span class="text-[#06B6D4]">"Recovery"</span></span> }.into_any()
                            } else if requires_totp_step.get() {
                                view! { <span>"Two-Factor "<span class="text-[#06B6D4]">"Verification"</span></span> }.into_any()
                            } else {
                                view! { <span>"Sign in to "<span class="text-[#06B6D4]">"PyraOcean"</span></span> }.into_any()
                            }
                        }}
                    </h1>
                    <p class="text-xs text-slate-400 mt-1">
                        {move || {
                            if show_forgot_password.get() {
                                "Reset your password via Recovery Code / 2FA or notify the Workspace SuperAdmin"
                            } else if requires_totp_step.get() {
                                "Enter the 6-digit code from your Authenticator app or your PYRA-... Recovery Code"
                            } else {
                                "Enter your enterprise credentials to access your Spaces and Dynamic Grids"
                            }
                        }}
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

                {move || {
                    info_msg.get().map(|msg| {
                        view! {
                            <div class="mb-4 p-3 rounded-lg bg-cyan-500/15 border border-cyan-500/40 text-xs text-cyan-300">
                                {msg}
                            </div>
                        }
                    })
                }}

                <Show
                    when=move || !show_forgot_password.get()
                    fallback=move || {
                        view! {
                            <div class="space-y-4">
                                <form class="space-y-3.5" on:submit=submit_self_service_reset>
                                    <div>
                                        <label class="block text-xs font-semibold uppercase tracking-wider text-slate-400 mb-1">
                                            "Account Email Address"
                                        </label>
                                        <input
                                            type="email"
                                            required
                                            placeholder="user@pyraocean.local"
                                            class="w-full px-3.5 py-2 rounded-lg bg-[#14171A]/90 border border-[#2A2F37] text-[#F1F5F9] text-sm focus:outline-none focus:border-[#06B6D4]"
                                            prop:value=email
                                            on:input=move |ev| email.set(event_target_value(&ev))
                                        />
                                    </div>

                                    <div class="p-3 rounded-xl bg-[#14171A]/70 border border-[#2A2F37] space-y-2">
                                        <div class="text-xs font-semibold text-slate-200">
                                            "Option 1: Request SuperAdmin Password Reset"
                                        </div>
                                        <p class="text-[11px] text-slate-400">
                                            "Flags your account in the SuperAdmin Control Panel so an administrator can issue a new password or recovery code."
                                        </p>
                                        <button
                                            type="button"
                                            disabled=move || is_loading.get()
                                            on:click=handle_notify_admin_reset
                                            class="w-full py-2 px-3 rounded-lg bg-[#06B6D4]/20 hover:bg-[#06B6D4]/30 border border-[#06B6D4]/40 text-[#06B6D4] font-semibold text-xs transition"
                                        >
                                            "🔑 Notify SuperAdmin for Password Reset"
                                        </button>
                                    </div>

                                    <div class="p-3 rounded-xl bg-[#14171A]/70 border border-[#2A2F37] space-y-2.5">
                                        <div class="text-xs font-semibold text-slate-200">
                                            "Option 2: Instant Self-Service Reset (2FA / Recovery Code)"
                                        </div>
                                        <div>
                                            <label class="block text-[11px] text-slate-400 mb-1">
                                                "6-Digit Authenticator Code or Recovery Code (PYRA-...)"
                                            </label>
                                            <input
                                                type="text"
                                                placeholder="123456 or PYRA-XXXX-XXXX-XXXX"
                                                class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs font-mono text-amber-300 focus:outline-none focus:border-[#06B6D4]"
                                                prop:value=recovery_code_input
                                                on:input=move |ev| recovery_code_input.set(event_target_value(&ev))
                                            />
                                        </div>
                                        <div>
                                            <label class="block text-[11px] text-slate-400 mb-1">
                                                "New Password (min 8 chars)"
                                            </label>
                                            <input
                                                type="password"
                                                placeholder="••••••••••••"
                                                class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                                prop:value=reset_new_password
                                                on:input=move |ev| reset_new_password.set(event_target_value(&ev))
                                            />
                                        </div>
                                        <button
                                            type="submit"
                                            disabled=move || is_loading.get()
                                            class="w-full py-2 px-3 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white font-semibold text-xs shadow transition"
                                        >
                                            "Reset Password Now"
                                        </button>
                                    </div>
                                </form>

                                <div class="text-center pt-1">
                                    <button
                                        type="button"
                                        on:click=move |_| {
                                            show_forgot_password.set(false);
                                            error_msg.set(None);
                                            info_msg.set(None);
                                        }
                                        class="text-xs text-[#06B6D4] hover:underline font-medium"
                                    >
                                        "← Back to Sign In"
                                    </button>
                                </div>
                            </div>
                        }
                    }
                >
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
                                on:input=move |ev| {
                                    email.set(event_target_value(&ev));
                                    requires_totp_step.set(false);
                                }
                            />
                        </div>

                        <div>
                            <div class="flex items-center justify-between mb-1.5">
                                <label class="block text-xs font-semibold uppercase tracking-wider text-slate-400">
                                    "Password"
                                </label>
                                <button
                                    type="button"
                                    on:click=move |_| {
                                        show_forgot_password.set(true);
                                        error_msg.set(None);
                                        info_msg.set(None);
                                    }
                                    class="text-[11px] text-[#06B6D4] hover:underline font-medium"
                                >
                                    "Forgot Password?"
                                </button>
                            </div>
                            <input
                                type="password"
                                required=move || !google_2fa_pending.get()
                                placeholder="••••••••••••"
                                class="w-full px-3.5 py-2.5 rounded-lg bg-[#14171A]/90 border border-[#2A2F37] text-[#F1F5F9] text-sm focus:outline-none focus:border-[#06B6D4]"
                                prop:value=password
                                on:input=move |ev| password.set(event_target_value(&ev))
                            />
                        </div>

                        <Show when=move || requires_totp_step.get()>
                            <div class="p-3.5 rounded-xl bg-slate-900/90 border border-[#06B6D4]/50 space-y-2">
                                <label class="block text-xs font-bold text-[#06B6D4]">
                                    "🔒 6-Digit Google Authenticator 2FA Code or Recovery Code"
                                </label>
                                <input
                                    type="text"
                                    autofocus
                                    placeholder="6-digit code (e.g. 123456) or PYRA-XXXX-..."
                                    class="w-full px-3.5 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-amber-300 font-mono text-sm tracking-wider focus:outline-none focus:border-[#06B6D4]"
                                    prop:value=totp_code
                                    on:input=move |ev| totp_code.set(event_target_value(&ev))
                                />
                            </div>
                        </Show>

                        <button
                            type="submit"
                            disabled=move || is_loading.get()
                            class="w-full py-2.5 px-4 rounded-lg bg-gradient-to-r from-[#F97316] to-[#EA580C] hover:from-[#EA580C] hover:to-[#F97316] text-white font-semibold text-sm shadow-lg transition disabled:opacity-50"
                        >
                            {move || {
                                if is_loading.get() {
                                    "Authenticating..."
                                } else if requires_totp_step.get() {
                                    "Verify 6-Digit Google Authenticator 2FA & Sign In"
                                } else {
                                    "Sign In to Workspace"
                                }
                            }}
                        </button>

                        <Show when=move || google_oauth_enabled.get()>
                            <div class="pt-2 space-y-3">
                                <div class="relative flex items-center justify-center">
                                    <div class="border-t border-[#2A2F37] w-full" />
                                    <span class="px-2.5 bg-[#1E2227] text-[10px] uppercase tracking-wider text-slate-400">
                                        "or"
                                    </span>
                                    <div class="border-t border-[#2A2F37] w-full" />
                                </div>

                                <button
                                    type="button"
                                    disabled=move || is_loading.get()
                                    on:click=move |_| {
                                        error_msg.set(None);
                                        info_msg.set(None);
                                        show_google_modal.set(true);
                                    }
                                    class="w-full py-2.5 px-4 rounded-lg bg-white hover:bg-slate-100 text-slate-900 font-semibold text-xs border border-slate-300 shadow flex items-center justify-center gap-2.5 transition"
                                >
                                    <span class="font-bold text-sm text-[#4285F4]">"G"</span>
                                    <span>"Sign in with Google"</span>
                                </button>
                            </div>
                        </Show>
                    </form>
                </Show>

                // Google OAuth2 / ID Token Sign-In Modal
                <Show when=move || show_google_modal.get()>
                    <div class="fixed inset-0 z-50 bg-black/75 flex items-center justify-center p-4">
                        <div class="w-full max-w-md bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 shadow-2xl space-y-4 text-left">
                            <div class="flex items-center justify-between">
                                <div class="flex items-center gap-2">
                                    <span class="w-6 h-6 rounded-full bg-white text-[#4285F4] font-bold text-xs flex items-center justify-center">
                                        "G"
                                    </span>
                                    <h3 class="text-sm font-bold text-white">
                                        "Sign in with Google Workspace (OAuth 2.0)"
                                    </h3>
                                </div>
                                <button
                                    type="button"
                                    on:click=move |_| show_google_modal.set(false)
                                    class="text-slate-400 hover:text-white"
                                >
                                    "✕"
                                </button>
                            </div>

                            <form class="space-y-3" on:submit=submit_google_login>
                                <div>
                                    <label class="block text-xs text-slate-300 mb-1">
                                        "Google Workspace Email"
                                    </label>
                                    <input
                                        type="email"
                                        required
                                        placeholder="you@pyramakerz.com"
                                        class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                        prop:value=email
                                        on:input=move |ev| email.set(event_target_value(&ev))
                                    />
                                </div>

                                <div>
                                    <label class="block text-[11px] text-slate-400 mb-1">
                                        "Google ID Token JWT (optional if authenticating by Workspace email)"
                                    </label>
                                    <input
                                        type="text"
                                        placeholder="eyJhbGciOiJSUzI1NiIs..."
                                        class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs font-mono text-slate-200 focus:outline-none focus:border-[#06B6D4]"
                                        prop:value=google_credential_input
                                        on:input=move |ev| google_credential_input.set(event_target_value(&ev))
                                    />
                                    {move || {
                                        let cid = google_client_id.get();
                                        if !cid.trim().is_empty() {
                                            view! {
                                                <div class="text-[10px] text-slate-500 font-mono mt-1 truncate">
                                                    "OAuth Client ID: " {cid}
                                                </div>
                                            }
                                            .into_any()
                                        } else {
                                            view! { <span /> }.into_any()
                                        }
                                    }}
                                </div>

                                <div class="pt-2 flex justify-end gap-2">
                                    <button
                                        type="button"
                                        on:click=move |_| show_google_modal.set(false)
                                        class="px-3 py-1.5 rounded-lg border border-[#2A2F37] text-xs text-slate-300"
                                    >
                                        "Cancel"
                                    </button>
                                    <button
                                        type="submit"
                                        class="px-4 py-1.5 rounded-lg bg-[#06B6D4] hover:bg-[#0284C7] text-slate-950 font-bold text-xs shadow transition"
                                    >
                                        "Continue with Google →"
                                    </button>
                                </div>
                            </form>
                        </div>
                    </div>
                </Show>

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

