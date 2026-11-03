use crate::{api::ApiClient, state::AuthState};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::{ImportGridResponse, ImportMode, PreviewImportResponse};
use uuid::Uuid;
use wasm_bindgen::JsCast;

#[component]
pub fn ImportModal(
    space_id: RwSignal<Option<Uuid>>,
    on_close: Callback<()>,
    on_imported: Callback<(Uuid, Uuid)>,
) -> impl IntoView {
    let auth = expect_context::<AuthState>();

    let custom_title = RwSignal::new(String::new());
    let selected_mode = RwSignal::new(ImportMode::FormattedSchemaInference);
    let selected_file = RwSignal::new(Option::<web_sys::File>::None);
    let selected_filename = RwSignal::new(String::new());
    let is_dragging = RwSignal::new(false);
    let is_previewing = RwSignal::new(false);
    let is_uploading = RwSignal::new(false);
    let error_msg = RwSignal::new(Option::<String>::None);
    let preview_data = RwSignal::new(Option::<PreviewImportResponse>::None);
    let completed_summary = RwSignal::new(Option::<ImportGridResponse>::None);

    let file_input_ref = NodeRef::<leptos::html::Input>::new();

    let run_preview = move |file: web_sys::File, mode: ImportMode| {
        let Some(sid) = space_id.get_untracked() else {
            return;
        };
        is_previewing.set(true);
        error_msg.set(None);
        spawn_local(async move {
            match ApiClient::preview_import_file(&auth, sid, mode, file).await {
                Ok(preview) => {
                    is_previewing.set(false);
                    preview_data.set(Some(preview));
                }
                Err(err) => {
                    is_previewing.set(false);
                    preview_data.set(None);
                    error_msg.set(Some(err));
                }
            }
        });
    };

    let on_file_picked = move |ev: web_sys::Event| {
        if let Some(target) = ev.target() {
            if let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() {
                if let Some(files) = input.files() {
                    if let Some(file) = files.get(0) {
                        selected_filename.set(file.name());
                        selected_file.set(Some(file.clone()));
                        completed_summary.set(None);
                        error_msg.set(None);
                        run_preview(file, selected_mode.get_untracked());
                    }
                }
            }
        }
    };

    let on_drop_file = move |ev: web_sys::DragEvent| {
        ev.prevent_default();
        is_dragging.set(false);
        if let Some(dt) = ev.data_transfer() {
            if let Some(files) = dt.files() {
                if let Some(file) = files.get(0) {
                    selected_filename.set(file.name());
                    selected_file.set(Some(file.clone()));
                    completed_summary.set(None);
                    error_msg.set(None);
                    run_preview(file, selected_mode.get_untracked());
                }
            }
        }
    };

    let submit_import = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let Some(sid) = space_id.get() else {
            return;
        };
        let Some(file) = selected_file.get() else {
            error_msg.set(Some(
                "Please select or drop a .csv, .json, .md, or .pdf file to import.".into(),
            ));
            return;
        };

        is_uploading.set(true);
        error_msg.set(None);

        let title_opt = {
            let t = custom_title.get().trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        };
        let mode = selected_mode.get();

        spawn_local(async move {
            match ApiClient::import_grid_file(&auth, sid, title_opt, mode, file).await {
                Ok(resp) => {
                    is_uploading.set(false);
                    let new_grid_id = resp.grid.id;
                    let new_grid = resp.grid.clone();
                    auth.space_grids.update(|map| {
                        map.entry(sid).or_default().push(new_grid);
                    });
                    completed_summary.set(Some(resp));
                    on_imported.run((sid, new_grid_id));
                }
                Err(err) => {
                    is_uploading.set(false);
                    error_msg.set(Some(err));
                }
            }
        });
    };

    view! {
        <Show when=move || space_id.get().is_some()>
            <div class="fixed inset-0 z-50 bg-black/75 flex items-center justify-center p-4">
                <div class="w-full max-w-3xl max-h-[90vh] overflow-y-auto bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 shadow-2xl space-y-5 text-[#F1F5F9]">
                    // Modal Header
                    <div class="flex items-center justify-between border-b border-[#2A2F37] pb-3">
                        <div>
                            <h3 class="text-lg font-bold text-white flex items-center gap-2">
                                <span class="text-[#06B6D4]">"⇄"</span>
                                <span>"Import from Notion / AppFlowy / PDF"</span>
                            </h3>
                            <p class="text-xs text-slate-400">
                                "Ingest CSV, JSON, Markdown, or coordinate-aligned PDF tables into a high-speed PyraOcean Grid"
                            </p>
                        </div>
                        <button
                            type="button"
                            on:click=move |_| {
                                completed_summary.set(None);
                                preview_data.set(None);
                                selected_file.set(None);
                                selected_filename.set(String::new());
                                on_close.run(());
                            }
                            class="text-slate-400 hover:text-white text-sm font-bold"
                        >
                            "✕"
                        </button>
                    </div>

                    {move || {
                        error_msg.get().map(|msg| {
                            view! {
                                <div class="p-3 rounded-xl bg-red-500/15 border border-red-500/40 text-xs text-red-300">
                                    {msg}
                                </div>
                            }
                        })
                    }}

                    // Live Completion Summary Banner
                    {move || {
                        completed_summary.get().map(|summary| {
                            view! {
                                <div class="p-4 rounded-xl bg-emerald-500/15 border border-emerald-500/40 space-y-1 text-xs text-emerald-200">
                                    <div class="font-bold text-sm text-emerald-300">
                                        "✓ Import Complete — " {summary.grid.title}
                                    </div>
                                    <div>
                                        {format!(
                                            "Created {} typed columns and imported {} rows.",
                                            summary.columns.len(),
                                            summary.imported_row_count
                                        )}
                                    </div>
                                </div>
                            }
                        })
                    }}

                    <form class="space-y-4" on:submit=submit_import>
                        // Optional Custom Grid Title
                        <div>
                            <label class="block text-xs font-semibold uppercase tracking-wider text-slate-400 mb-1">
                                "Target Grid Title (Optional — defaults to filename)"
                            </label>
                            <input
                                type="text"
                                placeholder="e.g. Q4 Product Roadmap"
                                class="w-full px-3.5 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                prop:value=custom_title
                                on:input=move |ev| custom_title.set(event_target_value(&ev))
                            />
                        </div>

                        // Drag-and-Drop Zone accepting `.csv`, `.json`, `.md`, and `.pdf`
                        <div>
                            <input
                                node_ref=file_input_ref
                                type="file"
                                accept=".csv,.json,.md,.markdown,.pdf,text/csv,application/json,text/markdown,application/pdf"
                                class="hidden"
                                on:change=on_file_picked
                            />
                            <div
                                on:dragover=move |ev: web_sys::DragEvent| {
                                    ev.prevent_default();
                                    is_dragging.set(true);
                                }
                                on:dragleave=move |_| is_dragging.set(false)
                                on:drop=on_drop_file
                                on:click=move |_| {
                                    if let Some(input) = file_input_ref.get() {
                                        input.click();
                                    }
                                }
                                class=move || {
                                    let border_cls = if is_dragging.get() {
                                        "border-[#06B6D4] bg-[#06B6D4]/10"
                                    } else {
                                        "border-[#2A2F37] bg-[#14171A]/70 hover:border-[#F97316]/60"
                                    };
                                    format!("p-6 rounded-xl border-2 border-dashed text-center cursor-pointer transition {border_cls}")
                                }
                            >
                                <div class="text-2xl mb-1">"📄"</div>
                                <div class="text-xs font-semibold text-white">
                                    {move || {
                                        let fname = selected_filename.get();
                                        if fname.is_empty() {
                                            "Drag & drop your Notion or AppFlowy export here, or click to browse".to_string()
                                        } else {
                                            format!("Selected File: {fname}")
                                        }
                                    }}
                                </div>
                                <p class="text-[11px] text-slate-400 mt-1">
                                    "Supports .csv (auto-detects ',', ';', or tab), .json arrays, .md Markdown tables, and .pdf documents"
                                </p>
                            </div>
                        </div>

                        // Import Mode Selector (Radio Cards)
                        <div class="space-y-2">
                            <span class="block text-xs font-semibold uppercase tracking-wider text-slate-400">
                                "Select Import Engine Mode"
                            </span>
                            <div class="grid grid-cols-1 sm:grid-cols-2 gap-3">
                                // Mode A: Formatted Import (Smart Schema Inference)
                                <div
                                    on:click=move |_| {
                                        selected_mode.set(ImportMode::FormattedSchemaInference);
                                        if let Some(f) = selected_file.get_untracked() {
                                            run_preview(f, ImportMode::FormattedSchemaInference);
                                        }
                                    }
                                    class=move || {
                                        let active = selected_mode.get() == ImportMode::FormattedSchemaInference;
                                        let cls = if active {
                                            "border-[#F97316] bg-[#F97316]/10"
                                        } else {
                                            "border-[#2A2F37] bg-[#14171A] hover:border-slate-600"
                                        };
                                        format!("p-3.5 rounded-xl border cursor-pointer transition space-y-1 {cls}")
                                    }
                                >
                                    <div class="flex items-center justify-between">
                                        <span class="text-xs font-bold text-white">
                                            "Formatted Import (Smart Schema)"
                                        </span>
                                        <span class="text-xs text-[#F97316] font-bold">
                                            {move || if selected_mode.get() == ImportMode::FormattedSchemaInference { "●" } else { "○" }}
                                        </span>
                                    </div>
                                    <p class="text-[11px] text-slate-400 leading-relaxed">
                                        "Automatically detects Dates, Numbers/Currencies, Checkboxes, Checklists, Multi-Select tags, Files, and People across sample rows."
                                    </p>
                                </div>

                                // Mode B: Unformatted Import (Raw Text Mode)
                                <div
                                    on:click=move |_| {
                                        selected_mode.set(ImportMode::UnformattedRawText);
                                        if let Some(f) = selected_file.get_untracked() {
                                            run_preview(f, ImportMode::UnformattedRawText);
                                        }
                                    }
                                    class=move || {
                                        let active = selected_mode.get() == ImportMode::UnformattedRawText;
                                        let cls = if active {
                                            "border-[#06B6D4] bg-[#06B6D4]/10"
                                        } else {
                                            "border-[#2A2F37] bg-[#14171A] hover:border-slate-600"
                                        };
                                        format!("p-3.5 rounded-xl border cursor-pointer transition space-y-1 {cls}")
                                    }
                                >
                                    <div class="flex items-center justify-between">
                                        <span class="text-xs font-bold text-white">
                                            "Unformatted Import (Raw Text)"
                                        </span>
                                        <span class="text-xs text-[#06B6D4] font-bold">
                                            {move || if selected_mode.get() == ImportMode::UnformattedRawText { "●" } else { "○" }}
                                        </span>
                                    </div>
                                    <p class="text-[11px] text-slate-400 leading-relaxed">
                                        "Strips Markdown and HTML tags and imports all columns strictly as plain Text using high-speed bulk SQL insertion."
                                    </p>
                                </div>
                            </div>
                        </div>

                        // Interactive Table Preview ("Preview & Import")
                        <Show when=move || is_previewing.get()>
                            <div class="p-3 rounded-xl bg-[#14171A] border border-[#2A2F37] text-xs text-slate-400 text-center">
                                "Analyzing layout & inferring column schema preview..."
                            </div>
                        </Show>

                        {move || {
                            preview_data.get().map(|prev| {
                                let cols = prev.columns.clone();
                                let srows = prev.sample_rows.clone();
                                view! {
                                    <div class="rounded-xl bg-[#14171A] border border-[#2A2F37] overflow-hidden space-y-0">
                                        <div class="px-3.5 py-2 border-b border-[#2A2F37] bg-[#111315] flex items-center justify-between text-[11px]">
                                            <span class="font-semibold text-[#06B6D4]">
                                                {format!(
                                                    "Table Preview — {} columns · {} total rows",
                                                    cols.len(),
                                                    prev.total_row_count
                                                )}
                                            </span>
                                            <span class="text-slate-400 font-mono">
                                                {format!("Showing first {} row(s)", srows.len())}
                                            </span>
                                        </div>
                                        <div class="overflow-x-auto max-h-52">
                                            <table class="w-full border-collapse text-left text-xs">
                                                <thead>
                                                    <tr class="border-b border-[#2A2F37] bg-[#181B20]">
                                                        {cols
                                                            .into_iter()
                                                            .map(|c| {
                                                                view! {
                                                                    <th class="px-3 py-2 border-r border-[#2A2F37] font-semibold text-white whitespace-nowrap">
                                                                        <div class="flex items-center gap-1.5">
                                                                            <span>{c.name}</span>
                                                                            <span class="px-1.5 py-0.2 rounded bg-[#06B6D4]/15 text-[#06B6D4] font-mono text-[9px] uppercase">
                                                                                {format!("{:?}", c.inferred_type)}
                                                                            </span>
                                                                        </div>
                                                                    </th>
                                                                }
                                                            })
                                                            .collect_view()}
                                                    </tr>
                                                </thead>
                                                <tbody class="divide-y divide-[#2A2F37]/60">
                                                    {srows
                                                        .into_iter()
                                                        .map(|r| {
                                                            view! {
                                                                <tr class="hover:bg-slate-800/30">
                                                                    {r.into_iter()
                                                                        .map(|cell_text| {
                                                                            view! {
                                                                                <td class="px-3 py-1.5 border-r border-[#2A2F37]/60 text-slate-300 whitespace-nowrap max-w-[220px] truncate">
                                                                                    {cell_text}
                                                                                </td>
                                                                            }
                                                                        })
                                                                        .collect_view()}
                                                                </tr>
                                                            }
                                                        })
                                                        .collect_view()}
                                                </tbody>
                                            </table>
                                        </div>
                                    </div>
                                }
                            })
                        }}

                        // Footer Buttons
                        <div class="pt-3 flex items-center justify-end gap-2 border-t border-[#2A2F37]">
                            <button
                                type="button"
                                on:click=move |_| {
                                    preview_data.set(None);
                                    on_close.run(());
                                }
                                class="px-3.5 py-2 rounded-lg border border-[#2A2F37] text-xs text-slate-300 hover:bg-slate-800"
                            >
                                "Close"
                            </button>
                            <button
                                type="submit"
                                disabled=move || is_uploading.get()
                                class="px-5 py-2 rounded-lg bg-gradient-to-r from-[#F97316] to-[#EA580C] hover:from-[#EA580C] hover:to-[#F97316] text-white text-xs font-semibold shadow-lg transition disabled:opacity-50"
                            >
                                {move || if is_uploading.get() { "Importing & Inferring Schema..." } else { "Confirm & Start Import →" }}
                            </button>
                        </div>
                    </form>
                </div>
            </div>
        </Show>
    }
}

