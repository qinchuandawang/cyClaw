use cyclaw_change_radar::{ChangeType, classify_path};

#[test]
fn api_route_file_should_be_classified_as_api_change() {
    let change_types = classify_path("src/api/project_knowledge_route.rs");

    assert!(change_types.contains(&ChangeType::Api));
}
