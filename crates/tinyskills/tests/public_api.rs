//! Public behavior checks for portable skill operations.

use std::fs;

use tinyskills::{
    BundledFile, BundledSkill, DiscoveryRoot, SkillFrontmatter, SkillScope, discover,
    inventory_resources, parse_skill_str, read_resource,
};

#[test]
fn parses_scalar_tool_lists_and_preserves_body() -> Result<(), Box<dyn std::error::Error>> {
    let source = "---\nname: demo\ndescription: useful\nallowed-tools: Bash, Read\n---\nDo it.\n";
    let (frontmatter, body, warnings) = parse_skill_str(source).ok_or("invalid document")?;
    assert_eq!(frontmatter.allowed_tools, ["Bash", "Read"]);
    assert_eq!(body, "Do it.\n");
    assert!(warnings.is_empty());
    Ok(())
}

fn frontmatter(yaml: &str) -> Result<SkillFrontmatter, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}

#[test]
fn allowed_tools_accepts_a_yaml_sequence() -> Result<(), Box<dyn std::error::Error>> {
    let fm = frontmatter("allowed-tools:\n  - Bash\n  - Read\n")?;
    assert_eq!(fm.allowed_tools, ["Bash", "Read"]);
    Ok(())
}

#[test]
fn allowed_tools_accepts_a_comma_joined_string() -> Result<(), Box<dyn std::error::Error>> {
    let fm = frontmatter("allowed-tools: Bash, Read, Grep, Skill, WebFetch")?;
    assert_eq!(
        fm.allowed_tools,
        ["Bash", "Read", "Grep", "Skill", "WebFetch"]
    );
    Ok(())
}

#[test]
fn allowed_tools_trims_whitespace_and_drops_empty_tokens() -> Result<(), Box<dyn std::error::Error>>
{
    let fm = frontmatter(r#"allowed-tools: " Bash, , Read, ""#)?;
    assert_eq!(fm.allowed_tools, ["Bash", "Read"]);
    Ok(())
}

#[test]
fn allowed_tools_accepts_the_tools_and_snake_case_aliases() -> Result<(), Box<dyn std::error::Error>>
{
    assert_eq!(
        frontmatter("tools: Bash, Read")?.allowed_tools,
        ["Bash", "Read"]
    );
    assert_eq!(
        frontmatter("allowed_tools: Bash, Read")?.allowed_tools,
        ["Bash", "Read"]
    );
    Ok(())
}

#[test]
fn allowed_tools_defaults_to_empty_when_absent() -> Result<(), Box<dyn std::error::Error>> {
    assert!(frontmatter("name: foo")?.allowed_tools.is_empty());
    Ok(())
}

#[test]
fn discovery_is_recursive_and_higher_scope_wins() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let user = temp.path().join("user/demo");
    let project = temp.path().join("project/nested/demo");
    write_skill(&user, "demo", "user copy")?;
    write_skill(&project, "demo", "project copy")?;

    let found = discover([
        DiscoveryRoot::new(temp.path().join("user"), SkillScope::User),
        DiscoveryRoot::new(temp.path().join("project"), SkillScope::Project),
    ]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].description, "project copy");
    assert_eq!(found[0].scope, SkillScope::Project);
    assert!(
        found[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("shadowed"))
    );
    Ok(())
}

#[test]
fn inventory_and_reads_ignore_symlinks() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let dir = temp.path().join("demo");
    write_skill(&dir, "demo", "description")?;
    fs::create_dir_all(dir.join("references"))?;
    fs::write(dir.join("references/guide.md"), "safe")?;
    let skills = discover([DiscoveryRoot::new(temp.path(), SkillScope::User)]);
    assert_eq!(
        inventory_resources(&dir),
        [std::path::PathBuf::from("references/guide.md")]
    );
    assert_eq!(
        read_resource(&skills[0], std::path::Path::new("references/guide.md"))?,
        "safe"
    );
    assert!(read_resource(&skills[0], std::path::Path::new("../outside")).is_err());
    Ok(())
}

#[test]
fn bundled_materialization_replaces_tampered_content() -> Result<(), Box<dyn std::error::Error>> {
    static FILES: &[BundledFile] = &[BundledFile {
        path: "SKILL.md",
        contents: "---\nname: demo\ndescription: bundled\n---\n",
    }];
    let bundle = BundledSkill {
        dir_name: "demo",
        files: FILES,
    };
    let temp = tempfile::tempdir()?;
    let first = tinyskills::install(temp.path(), &[bundle]);
    assert_eq!(first.written, ["demo"]);
    fs::write(temp.path().join("demo/SKILL.md"), "tampered")?;
    let second = tinyskills::install(temp.path(), &[bundle]);
    assert_eq!(second.written, ["demo"]);
    assert!(tinyskills::is_current_materialization(
        &temp.path().join("demo"),
        bundle
    ));
    Ok(())
}

fn write_skill(dir: &std::path::Path, name: &str, description: &str) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\nBody\n"),
    )
}
