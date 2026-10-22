mod carve;
mod extract;
mod text;
mod commands;

use commands::{
    list_devices, scan_blocks, cancel_block_scan, open_found_file, reveal_found_file,
    read_found_text, zip_entries, search_files, cancel_search, find_custom,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            list_devices,
            scan_blocks,
            cancel_block_scan,
            open_found_file,
            reveal_found_file,
            read_found_text,
            zip_entries,
            search_files,
            cancel_search,
            find_custom,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
