<div align="center">

# 🌊 PyraOcean

**A High-Performance, Self-Hosted Collaborative Workspace & Dynamic Database Platform Built 100% in Rust**

[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![Backend](https://img.shields.io/badge/Backend-Axum%20%2B%20SQLx-06B6D4)](https://github.com/tokio-rs/axum)
[![Frontend](https://img.shields.io/badge/Frontend-Leptos%20(WASM)-EF4444)](https://leptos.dev/)
[![Database](https://img.shields.io/badge/Database-PostgreSQL%2016%20(JSONB)-336791?logo=postgresql)](https://www.postgresql.org/)
[![Docker](https://img.shields.io/badge/Deployment-Docker%20Compose-2496ED?logo=docker)](https://www.docker.com/)

</div>

---

## 📖 Overview

**PyraOcean** is an enterprise-grade, self-hosted workspace and relational grid database inspired by **Notion** and **AppFlowy**, engineered from the ground up in **100% Rust** (both backend API and WebAssembly frontend). 

Designed for organizations that require complete data sovereignty, sub-millisecond responsiveness, and strict Role-Based Access Control (RBAC), PyraOcean combines the flexibility of Notion's dynamic properties with the spreadsheet power of Excel—all running in a lightweight, memory-safe footprint without external JavaScript runtime bloat or heavy third-party caching containers.

---

## ✨ Key Features

### 🗄️ 1. Dynamic 11-Property Grid Engine
Every Grid page supports **11 rich, dynamically typed column properties** backed by indexed PostgreSQL `JSONB` storage:
* **Text** — Rich inline text editing.
* **Number** — Numeric values and currency formatting (`EGP`, `USD`, etc.).
* **Select** — Single-choice colored status badges with inline option creation, renaming, color customization, and deletion.
* **MultiSelect** — Multi-tag colored badges with on-the-fly tag CRUD management.
* **Date** — Native calendar date picker (`YYYY-MM-DD`).
* **Time** — Time-of-day selector (`HH:MM`).
* **Files & Media** — Direct multi-file cell attachments (up to 50 MB per upload) with instant preview and download links.
* **Checkbox** — Interactive boolean toggle (`true` / `false`).
* **Checklist** — Interactive multi-item sub-task list with progress tracking inside a single cell.
* **Last Modified** — Automated, database-triggered timestamp tracking the exact moment and user of the latest row update.
* **Person** — Space member assignment picker.

### ⚡ 2. Spreadsheet & Notion Power-User UX
* **Excel-Style Fill Handle (Drag-to-Copy):** Select any cell and drag the bottom-right cyan fill handle vertically or horizontally across adjacent cells to batch-copy and auto-cast values.
* **Free Drag-and-Drop Column & Row Reordering:** Rearrange columns horizontally or drag rows vertically via `⋮⋮` grip handles with instant persistence.
* **Hover `+` Positional Row Insertion:** Hover over any row to reveal a subtle `+` button at the start of the row, allowing instant insertion **Above** or **Below** the current row, alongside the inline bottom `+ New Row` bar.
* **Independent Column Width Resizing & Layout Reset:** Drag column borders smoothly at 60fps (`table-layout: fixed`) without shifting neighboring columns, or click **Reset Layout** to snap all widths back to defaults.
* **Dynamic Column Property Conversion:** Change any column's property type on the fly with automatic server-side JSONB data casting.
* **Hover-to-Copy Cell Action:** One-click cell value copying with automatic fallback support for both HTTPS and local LAN HTTP environments.
* **Multi-Step Undo Engine (`Ctrl+Z`):** Revert cell edits, bulk drag-fills, row deletions, column resizes, and reordering instantly.
* **Pinned Horizontal Scrollbar & Viewport-Aware Popovers:** Full-viewport locked layout keeps the horizontal slider accessible at all times, while `position: fixed` smart popovers automatically flip upward near screen edges so menus are never clipped.

### 🛡️ 3. 3-Tier Role-Based Access Control (RBAC)
* **Super Admin (Singleton):** Full system control, global user management, Space creation, complete audit log visibility, and cross-workspace search.
* **Space Admin:** Manages assigned Spaces, configures member permissions, and oversees Space-level Grids and Trash.
* **Standard User:** Granular per-Space and per-Grid access levels (`Can Edit` vs. `Can View`).

### 🔄 4. Universal Importer & Executive Exporter
* **Multi-Format Importer (Notion & AppFlowy Compatible):**
  * Supports **CSV (`.csv`)**, **JSON (`.json`)**, **Markdown Tables (`.md`)**, and **PDF Documents (`.pdf`)**.
  * **Layout-Aware PDF Table Engine:** Uses `pdftotext -layout` (`poppler-utils`) + coordinate boundary detection to accurately reconstruct multi-column PDF tables without scrambling columns or splitting currency symbols.
  * Choose between **Formatted Import** (automatic 11-property schema inference) or **Unformatted Import** (raw text ingestion).
* **Multi-Format Exporter:**
  * **CSV (`.csv`):** RFC 4180 compliant with UTF-8 BOM for seamless Microsoft Excel & LibreOffice compatibility.
  * **Markdown (`.md`):** Clean GitHub-Flavored Markdown tables.
  * **Executive A3 Landscape PDF (`.pdf`):** High-contrast, print-ready A3 Landscape vector PDF reports with preserved tag colors and currency highlights.

### 🕒 5. 30-Day Version History & Soft-Delete Trash System
* **30-Day Point-in-Time History:** Non-blocking background snapshots record grid changes over a rolling 30-day window. Click any historical entry to inspect the entire grid in a **Full-Page Read-Only Preview Modal** before restoring.
* **Dedicated Trash Manager (`/trash`):** Deleted grids are soft-deleted to the Trash page for 30 days with countdown badges, Space filters, one-click **Restore**, **Permanent Delete**, and **Empty Trash** actions.

### 🔍 6. 3-Scope Instant Search (`Ctrl+K`)
* Search instantaneously across **`[This Grid]`**, **`[This Space]`**, or **`[All Spaces]`** with RBAC-filtered PostgreSQL `JSONB` indexing.

---

## 🏗️ Architecture & Tech Stack

PyraOcean is organized as a Cargo Workspace with three crates:

```text
PyraOcean/
├── shared/          # Isomorphic DTOs, 11 PropertyType enums, RBAC models & serializers
├── backend/         # Axum HTTP/REST server, SQLx PostgreSQL pool, DashMap cache, PDF/CSV Importer
├── frontend/        # Leptos 0.7 CSR WebAssembly SPA, Tailwind CSS, reactive signals
├── migrations/      # SQLx PostgreSQL schema, JSONB GIN indexes & automated triggers
├── Dockerfile       # Multi-stage production container build
└── docker-compose.yml