use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use crate::error::WorkflowError;
use crate::workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Selection {
    Repository,
    Target(String),
    Component { target: String, component: String },
    Path(PathBuf),
}

pub fn resolve_selectors(
    workspace: &Workspace,
    selectors: &[String],
) -> Result<Vec<Selection>, WorkflowError> {
    if selectors.is_empty() {
        return Ok(vec![Selection::Repository]);
    }

    let mut seen = HashSet::new();
    let mut resolved = Vec::new();
    for selector in selectors {
        let selection = if selector.starts_with('@') {
            resolve_at_selector(workspace, selector)?
        } else {
            resolve_path_selector(workspace, selector)?
        };
        if seen.insert(selection.clone()) {
            resolved.push(selection);
        }
    }
    Ok(resolved)
}

fn resolve_at_selector(workspace: &Workspace, selector: &str) -> Result<Selection, WorkflowError> {
    let body = selector.strip_prefix('@').unwrap_or_default();
    let mut parts = body.split(':');
    let target_name = parts.next().unwrap_or_default();
    let component_name = parts.next();
    if target_name.is_empty()
        || !valid_identifier(target_name)
        || component_name
            .is_some_and(|component| component.is_empty() || !valid_identifier(component))
        || parts.next().is_some()
    {
        return Err(WorkflowError::Selector(format!(
            "invalid selector '{selector}'; expected @target or @target:component"
        )));
    }

    let target = workspace
        .config
        .targets
        .get(target_name)
        .ok_or_else(|| WorkflowError::Selector(format!("unknown target '@{target_name}'")))?;
    match component_name {
        None => Ok(Selection::Target(target_name.to_owned())),
        Some(component_name) => {
            let exists = target
                .components
                .iter()
                .any(|component| component.name.as_deref() == Some(component_name));
            if !exists {
                return Err(WorkflowError::Selector(format!(
                    "target '@{target_name}' has no component '{component_name}'"
                )));
            }
            Ok(Selection::Component {
                target: target_name.to_owned(),
                component: component_name.to_owned(),
            })
        }
    }
}

fn resolve_path_selector(
    workspace: &Workspace,
    selector: &str,
) -> Result<Selection, WorkflowError> {
    let path = PathBuf::from(selector);
    if selector.is_empty() || path.is_absolute() {
        return Err(WorkflowError::Selector(format!(
            "path selector '{selector}' must be a nonempty invocation-relative path"
        )));
    }
    let candidate = workspace.invocation.join(path);
    let resolved = fs::canonicalize(&candidate).map_err(|_| {
        WorkflowError::Selector(format!(
            "selector path does not exist: '{}'",
            candidate.display()
        ))
    })?;
    if !resolved.starts_with(&workspace.root) {
        return Err(WorkflowError::Selector(format!(
            "selector path escapes the workspace: '{}'",
            candidate.display()
        )));
    }
    Ok(Selection::Path(resolved))
}

fn valid_identifier(identifier: &str) -> bool {
    let mut characters = identifier.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
        && characters
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}
