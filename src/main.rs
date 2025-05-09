// src/main.rs

use serde::Deserialize;
// Slint imports
use slint::{Model, ModelRc, SharedPixelBuffer, VecModel, Rgba8Pixel, Timer, TimerMode, ComponentHandle, SharedString };
// Standard library imports
use std::rc::Rc;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use std::vec::Vec;
use std::path::{Path, PathBuf};
use std::fs;
// Tokio imports
use tokio::sync::mpsc;
use tokio::runtime::Handle;
// Other Crates
use reqwest;
use url::Url;
use image::ImageError;
use webbrowser;
use serde_json;
use directories::ProjectDirs;

// --- API Data Structures ---
#[derive(Deserialize, Debug, Clone)]
struct CoinGeckoImageUrls {
    #[allow(dead_code)] thumb: String,
    // small: String, // Removed based on previous code
    large: String,
}
#[derive(Deserialize, Debug, Clone)]
struct CoinGeckoMarketData {
    current_price: HashMap<String, f64>,
    price_change_percentage_24h: Option<f64>,
}
#[derive(Deserialize, Debug, Clone)]
struct CoinGeckoCoinResponse {
    id: String,
    symbol: String,
    name: String,
    image: CoinGeckoImageUrls,
    market_data: CoinGeckoMarketData,
}

// --- Slint Generated Code Inclusion ---
slint::include_modules!();

// --- Error Type Alias ---
type AppError = Box<dyn std::error::Error + Send + Sync>;

// --- Message Type for Channel ---
#[derive(Debug, Clone)]
struct PreparedCoinData {
    id: String,
    name: String,
    symbol: String,
    price_usd: f64,
    change_percent_24h: Option<f64>,
    image_data_result: Result<(Vec<u8>, u32, u32), String>,
}

// --- Async Fetch Functions ---
async fn fetch_coin_data(coin_id: &str) -> Result<(String, String, String, f64, Option<f64>, String), AppError> {
    let request_url = format!(
        "https://api.coingecko.com/api/v3/coins/{}?localization=false&tickers=false&market_data=true&community_data=false&developer_data=false&sparkline=false",
        coin_id
    );
    println!("[FETCH] Coin data from CoinGecko: {}", request_url);
    let client = reqwest::Client::new();
    let response = client.get(&request_url)
        .header("User-Agent", "RustyCoinTracker/0.1") // Good practice User-Agent
        .send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_else(|_| format!("Status {}", status));
        return Err(format!("CoinGecko API req failed for '{}': {}", coin_id, text).into());
    }
    let api_response = response.json::<CoinGeckoCoinResponse>().await?;
    let id = api_response.id;
    let name = api_response.name;
    let symbol = api_response.symbol.to_uppercase();
    let price_usd_f64 = api_response.market_data.current_price.get("usd").cloned().unwrap_or(0.0);
    let change_percent_opt_f64 = api_response.market_data.price_change_percentage_24h;
    let icon_url = api_response.image.large;
    println!("[FETCH] Found icon URL: {}", icon_url);
    Ok((id, name, symbol, price_usd_f64, change_percent_opt_f64, icon_url))
}

async fn fetch_and_prepare_image_data(url: &str) -> Result<(Vec<u8>, u32, u32), AppError> {
    println!("[FETCH] Image from: {}", url);
    if Url::parse(url).is_err() { return Err(format!("Invalid image URL: {}", url).into()); }
    let client = reqwest::Client::new();
    let response = client.get(url).send().await.map_err(|e| format!("Reqwest send error: {}", e))?;
    if !response.status().is_success() { return Err(format!("Image fetch failed: {}", response.status()).into()); }
    let image_bytes = response.bytes().await.map_err(|e| format!("Reqwest bytes error: {}", e))?;
    println!("[FETCH] Image bytes: {}", image_bytes.len());
    let decoded_image = image::load_from_memory(&image_bytes).map_err(|e: ImageError| format!("Image decode error: {}", e))?;
    let rgba_image = decoded_image.to_rgba8();
    let (width, height) = rgba_image.dimensions();
    if width == 0 || height == 0 { return Err("Invalid image dimensions".into()); }
    println!("[FETCH] Decoded image dimensions: {}x{}", width, height);
    Ok((rgba_image.into_raw(), width, height))
}

// --- Reusable Async Task Logic ---
async fn fetch_and_send_coin_update(coin_id: String, tx: mpsc::Sender<PreparedCoinData>) {
    println!("[ASYNC TASK] Starting fetch for: {}", coin_id);
    match fetch_coin_data(&coin_id).await {
        Ok((id, name, symbol, price_usd, change_percent_24h, icon_url_string)) => {
            println!("[ASYNC TASK] Data fetched for: {}", name);
            let image_data_result = fetch_and_prepare_image_data(&icon_url_string).await;
            let image_data_for_channel = image_data_result.map_err(|e| e.to_string());
            let prepared_data = PreparedCoinData { id, name, symbol, price_usd, change_percent_24h, image_data_result: image_data_for_channel };
            if let Err(_) = tx.send(prepared_data).await { eprintln!("[ASYNC TASK SEND ERROR] Failed for {}", coin_id); }
            else { println!("[ASYNC TASK SEND OK] Sent data for {}.", coin_id); }
        }
        Err(e) => { eprintln!("[ASYNC TASK FETCH ERROR] Failed for {}: {}", coin_id, e); }
    }
}

// --- Persistence Helper Functions ---
fn get_config_path() -> Option<PathBuf> {
    if let Some(proj_dirs) = ProjectDirs::from("com", "RustyCoinDev", "RustyCoinTracker") {
        let config_dir = proj_dirs.config_dir();
        if let Err(e) = fs::create_dir_all(config_dir) {
            eprintln!("[ERROR] Could not create config directory {}: {}", config_dir.display(), e); None
        } else { Some(config_dir.join("tracked_coins.json")) }
    } else { eprintln!("[ERROR] Could not determine config directory."); None }
}

fn load_tracked_ids(path: &Path) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(json_data) => {
            match serde_json::from_str::<Vec<String>>(&json_data) {
                Ok(ids) => { println!("[LOAD] Loaded {} IDs from {}", ids.len(), path.display()); ids }
                Err(e) => { eprintln!("[ERROR] Parsing {}: {}", path.display(), e); Vec::new() }
            }
        }
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => { println!("[LOAD] Not found: {}", path.display()); Vec::new() }
        Err(e) => { eprintln!("[ERROR] Reading {}: {}", path.display(), e); Vec::new() }
    }
}

fn save_tracked_ids(path: &Path, ids: &Vec<String>) {
    match serde_json::to_string_pretty(ids) {
        Ok(json_data) => {
            if let Err(e) = fs::write(path, json_data) { eprintln!("[ERROR] Writing {}: {}", path.display(), e); }
            else { println!("[SAVE] Saved {} IDs to {}", ids.len(), path.display()); }
        }
        Err(e) => { eprintln!("[ERROR] Serializing IDs: {}", e); }
    }
}

fn get_current_ids_from_model(model: &Rc<VecModel<CoinData>>) -> Vec<String> {
    model.iter().map(|c| c.id.to_string()).collect()
}

// Define cooldown period
const REFRESH_COOLDOWN: Duration = Duration::from_secs(2);

// --- Main Application Entry Point ---
#[tokio::main]
async fn main() -> Result<(), slint::PlatformError> {
    let config_path_opt = get_config_path();
    let ui = AppWindow::new()?;
    let coins_model_rc = Rc::new(VecModel::<CoinData>::from(Vec::new()));
    ui.set_tracked_coins(ModelRc::from(coins_model_rc.clone()));
    let (tx, mut rx) = mpsc::channel::<PreparedCoinData>(10);
    let model_handle = coins_model_rc.clone();
    let tx_handle = tx.clone();
    let tokio_handle = Handle::current();
    let mut initial_ids = Vec::new();
    if let Some(ref config_path) = config_path_opt { initial_ids = load_tracked_ids(config_path); }

    // State for Refresh Debounce Timestamp
    let last_refresh_time = Rc::new(RefCell::new(None::<Instant>));
    // State for tracking active fetch tasks
    let active_tasks = Rc::new(RefCell::new(0usize));

    // --- Handle UI Request to Add Coin ---
    ui.on_add_coin({
        let ui_handle_add = ui.as_weak();
        let coins_model_rc_check = model_handle.clone();
        let tx_add = tx_handle.clone();
        let tokio_h_add = tokio_handle.clone();
        let active_tasks_add = active_tasks.clone();

        move |coin_id| {
            let coin_id = coin_id.trim().to_lowercase(); if coin_id.is_empty() { return; }
            let coin_id_str = coin_id.clone();
            if coins_model_rc_check.iter().any(|c| c.id.as_str() == coin_id_str) { println!("[UI] Coin '{}' is already tracked.", coin_id_str); return; }

            // Increment counter & set state BEFORE spawning
            if let Some(ui_strong) = ui_handle_add.upgrade() {
                 let mut count = active_tasks_add.borrow_mut();
                 *count += 1;
                 println!("[ADD] Incremented task count. Active: {}", *count);
                 if !ui_strong.get_is_refreshing() {
                     ui_strong.set_is_refreshing(true);
                     // Note: Removed explicit animation start call here, relying on Slint state binding
                 }
            } else { return; } // Don't spawn if UI is gone

            println!("[SPAWN] Add task: {}", coin_id_str);
            tokio_h_add.spawn(fetch_and_send_coin_update(coin_id_str, tx_add.clone()));
        }
    });

    // --- Handle UI Request to Remove Coin ---
    let model_handle_remove = model_handle.clone();
    let config_path_remove = config_path_opt.clone();
    ui.on_remove_coin(move |coin_id| {
        let id_str = coin_id.to_string(); println!("[UI] Remove coin '{}'", id_str);
        if let Some(index) = model_handle_remove.iter().position(|c| c.id.as_str() == id_str) {
            model_handle_remove.remove(index); println!("[UI] Removed {}. Count: {}", id_str, model_handle_remove.row_count());
             if let Some(ref path) = config_path_remove { let current_ids = get_current_ids_from_model(&model_handle_remove); save_tracked_ids(path, &current_ids); }
        } else { println!("[UI] Remove: {} not found.", id_str); }
    });

    // --- Handle UI Request to Refresh All Coins (with Debounce & State) ---
    ui.on_refresh_all({
        let ui_handle_refresh = ui.as_weak();
        let model_handle_refresh = model_handle.clone();
        let tx_refresh = tx_handle.clone();
        let tokio_h_refresh = tokio_handle.clone();
        let last_refresh_time_clone = last_refresh_time.clone();
        let active_tasks_refresh = active_tasks.clone();

        move || {
            println!("[UI] Refresh All triggered.");

            // Check if already refreshing (using the counter)
            // Check MUST happen before borrowing RefCell for last_time
            if *active_tasks_refresh.borrow() > 0 {
                 println!("[REFRESH] Already refreshing ({} tasks active). Ignoring.", *active_tasks_refresh.borrow());
                 return;
            }

            if let Some(ui) = ui_handle_refresh.upgrade() {
                // Debounce Check
                let now = Instant::now();
                let mut last_time = last_refresh_time_clone.borrow_mut();
                if let Some(last) = *last_time {
                    if now.saturating_duration_since(last) < REFRESH_COOLDOWN {
                        println!("[REFRESH] Cooldown active. Wait {:.1}s", (REFRESH_COOLDOWN - now.saturating_duration_since(last)).as_secs_f32());
                        return; // Just exit early
                    }
                }
                // Update timestamp if proceeding
                *last_time = Some(now);

                let current_ids = get_current_ids_from_model(&model_handle_refresh);
                if current_ids.is_empty() {
                    println!("[REFRESH] No coins to refresh.");
                    // Don't need to change is_refreshing or reset cooldown if nothing happens
                    return;
                }

                // Set loading state and task count
                let tasks_to_spawn = current_ids.len();
                println!("[REFRESH] Starting refresh for {} coins.", tasks_to_spawn);
                ui.set_is_refreshing(true); // Start loading indicator/spinner state
                *active_tasks_refresh.borrow_mut() = tasks_to_spawn; // SET task count
                println!("[REFRESH] Set task count. Active: {}", *active_tasks_refresh.borrow());

                // Spawn tasks
                println!("[REFRESH] Spawning tasks for {} coins.", current_ids.len());
                for coin_id in current_ids {
                    tokio_h_refresh.spawn(fetch_and_send_coin_update(coin_id, tx_refresh.clone()));
                }
            } else {
                 println!("[REFRESH] UI handle dropped.");
            }
        }
    });

    // --- Handle Open URL ---
    ui.on_open_coin_link(move |coin_id| {
        let id_str = coin_id.to_string(); println!("[UI] Open URL for '{}'", id_str);
        let target_url = format!("https://www.coingecko.com/en/coins/{}", id_str);
        println!("[BROWSER] Opening URL: {}", target_url);
        tokio::task::spawn_blocking(move || {
            match webbrowser::open(&target_url) { Ok(_) => {}, Err(e) => eprintln!("[BROWSER] Failed open: {}", e), }
        });
    });

    // --- Main Thread Receiver Loop (Stop animation based on counter) ---
    let timer = Timer::default();
    let main_model_rc = model_handle.clone();
    let config_path_receiver = config_path_opt.clone();
    let ui_handle_receiver = ui.as_weak();
    let active_tasks_receiver = active_tasks.clone();

    timer.start(TimerMode::Repeated, std::time::Duration::from_millis(50), move || {
        let mut received_this_tick = 0;
        let mut should_stop_refreshing = false;
        let mut current_task_count_after_recv: usize = 0; // Track count *after* decrementing

        while let Ok(prepared_data) = rx.try_recv() {
            received_this_tick += 1;
            println!("[RECV OK] Received data for {}", prepared_data.name);

            // Decrement active task counter - MUST happen for every message received
            { // Scope for RefCell borrow
                let mut active_count = active_tasks_receiver.borrow_mut();
                if *active_count > 0 {
                    *active_count -= 1;
                    current_task_count_after_recv = *active_count; // Store current count
                    println!("[RECV] Decremented active tasks. Remaining: {}", current_task_count_after_recv);
                    if current_task_count_after_recv == 0 {
                        // Mark to stop refreshing only if the counter *just now* hit zero
                        should_stop_refreshing = true;
                    }
                } else {
                     // This could happen if an Add task finishes after a Refresh was cancelled/completed
                     println!("[RECV] Warning: Received data but task count already zero?");
                }
            }

            // Process data and update model
            let final_icon = match prepared_data.image_data_result { Ok((d,w,h)) => slint::Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&d,w,h)), Err(e) => {eprintln!("[RECV] ImgErr {}: {}", prepared_data.name,e);slint::Image::default()} };
            let formatted_price = format!("{:.2}", prepared_data.price_usd);
            let mut formatted_change = String::from("-.--%"); let mut is_up = false; let mut is_down = false;
            if let Some(change_f64) = prepared_data.change_percent_24h { formatted_change = format!("{:.2}%", change_f64); if change_f64 > 0.0 { is_up = true; formatted_change = format!("+{}", formatted_change); } else if change_f64 < 0.0 { is_down = true; } else { formatted_change = format!("+{}", formatted_change); } }
            let coin_name_for_sort = prepared_data.name.clone();
            let new_or_updated_coin_data = CoinData { id: prepared_data.id.clone().into(), name: prepared_data.name.into(), symbol: prepared_data.symbol.into(), price: formatted_price.into(), icon: final_icon, change_percent: formatted_change.into(), is_up, is_down, };
            let target_id : SharedString = prepared_data.id.into();
            let mut added_new = false;
            if let Some(index) = main_model_rc.iter().position(|c| c.id == target_id) { main_model_rc.set_row_data(index, new_or_updated_coin_data); println!("[RECV] Updated {}. Count: {}", target_id, main_model_rc.row_count()); }
            else { let insertion_index = main_model_rc.iter().position(|c| { c.name.to_lowercase() >= coin_name_for_sort.to_lowercase() }); if let Some(index) = insertion_index { main_model_rc.insert(index, new_or_updated_coin_data); } else { main_model_rc.push(new_or_updated_coin_data); } println!("[RECV] Added new {}. Count: {}", target_id, main_model_rc.row_count()); added_new = true; }
            if added_new { if let Some(ref path) = config_path_receiver { let ids = get_current_ids_from_model(&main_model_rc); save_tracked_ids(path, &ids); } }

            // Limit work per tick
            if received_this_tick > 10 { break; }
        } // End while let Ok...

        // Stop loading indicator AFTER processing messages if counter hit zero
         if should_stop_refreshing {
             if let Some(ui) = ui_handle_receiver.upgrade() {
                 println!("[RECV] All tasks complete. Setting is_refreshing=false");
                 ui.set_is_refreshing(false); // Stop loading indicator / Enable button
             }
         }

        // Check disconnection (optional)
        if received_this_tick == 0 { if let Err(e) = rx.try_recv() { if !matches!(e, mpsc::error::TryRecvError::Empty) { eprintln!("[RECV ERROR] MPSC channel likely disconnected! Error: {:?}", e); } } }

    }); // End Timer closure

    // --- Trigger Initial Fetches (Set is_refreshing) ---
    if !initial_ids.is_empty() {
         if let Some(ui) = ui.as_weak().upgrade() {
            let num_initial = initial_ids.len();
            println!("[INIT] Setting task count to {} and starting loading indicator.", num_initial);
            *active_tasks.borrow_mut() = num_initial; // Initialize count
            ui.set_is_refreshing(true); // Start loading indicator
         }
         println!("[INIT] Triggering fetch for {} saved coins.", initial_ids.len());
         for coin_id in initial_ids { tokio_handle.spawn(fetch_and_send_coin_update(coin_id, tx_handle.clone())); }
    }

    println!("Starting Slint event loop...");
    ui.run()
} // End main