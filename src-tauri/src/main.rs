#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod acp;
mod ai_plan;
mod api_planning;
mod catalog;
mod chatgpt;
mod chatgpt_storage;
mod delivery;
mod domain;
mod module_preferences;
mod network;
mod planner;
mod preview;
mod project;
mod refinement;
mod skill_catalog;
mod validation;
mod workspace;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            use tauri::Manager;
            if let Ok(directory) = app.path().app_data_dir() {
                module_preferences::initialize(directory.clone());
                skill_catalog::initialize(directory);
            }
            Ok(())
        })
        .manage(refinement::SessionKey::default())
        .manage(api_planning::ApiSession::default())
        .manage(chatgpt::ChatGptSession::default())
        .invoke_handler(tauri::generate_handler![
            domain::validate_profile,
            catalog::load_catalog,
            skill_catalog::sync_skill_catalog,
            skill_catalog::skill_catalog_file,
            skill_catalog::open_skill_catalog_file,
            catalog::open_module_source,
            project::inspect_project,
            project::open_project_directory,
            planner::recommend,
            ai_plan::evaluate_ai_plan,
            api_planning::configure_api,
            api_planning::remember_api,
            api_planning::restore_api,
            api_planning::set_api_model,
            api_planning::list_provider_models,
            api_planning::openrouter_prices,
            api_planning::test_api,
            api_planning::discover_project,
            api_planning::request_plan,
            module_preferences::read_module_preferences,
            module_preferences::save_module_preferences,
            api_planning::translate_module_description,
            acp::open_acp_login,
            acp::check_acp_connection,
            chatgpt::connect_chatgpt,
            chatgpt::restore_chatgpt,
            chatgpt::disconnect_chatgpt,
            chatgpt::test_chatgpt,
            validation::validate_plan,
            delivery::generate_configuration,
            preview::preview_configuration,
            preview::check_preview,
            preview::pick_project_directory,
            preview::validate_project_directory,
            preview::create_project,
            delivery::prepare_agent_instructions,
            delivery::prepare_agent_export,
            delivery::save_agent_export,
            workspace::encode_project_yaml,
            workspace::decode_project_yaml,
            workspace::save_project_history,
            workspace::list_project_history,
            workspace::remove_project_history,
            workspace::open_project_history,
            workspace::open_project_snapshot,
            preview::apply_configuration,
            refinement::preview_refinement,
            refinement::set_cloud_key,
            refinement::refine_plan
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Prelude");
}
