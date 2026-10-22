use std::collections::{HashMap, HashSet};

use anyhow::Result;
use console::{style, Key, Term};
use dialoguer::Confirm;

use crate::manifest::{Component, Manifest};
use crate::state::State;

#[derive(Clone, Copy)]
pub enum MainMenuAction {
    ManageComponents,
    SyncSpecific,
    UninstallSpecific,
    ChangeEnvironment,
    UpdateRepo,
    Status,
    Exit,
}

pub struct ComponentSelection {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
    pub reinstall: bool,
    pub skip_ask: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectionMode {
    /// Full declarative state: pre-selects enabled/default; unchecked items are uninstalled.
    Declarative,
    /// Targeted sync/install: starts empty; only selected items are synced, nothing is uninstalled.
    SyncSpecific,
    /// Targeted uninstall: starts empty; only selected items are uninstalled, nothing is synced.
    UninstallSpecific,
}

/// Clear visible screen, scrollback buffer, and print a clean TUI header.
pub fn render_header(term: &Term, subtitle: &str) -> Result<()> {
    print!("\x1b[2J\x1b[3J\x1b[H");
    term.flush()?;
    println!(
        "  {} {}",
        style("waysync").cyan().bold(),
        style(format!("── {}", subtitle)).dim()
    );
    println!(
        "  {}\n",
        style("────────────────────────────────────────────────────────").dim()
    );
    Ok(())
}

/// Wait for the user to press Enter/Space/Esc before clearing the screen again.
pub fn wait_for_enter() -> Result<()> {
    let term = Term::stdout();
    println!(
        "\n  {}",
        style("Press Enter to return to the menu...").dim()
    );
    loop {
        match term.read_key()? {
            Key::Enter | Key::Escape | Key::Char(' ') | Key::Char('q') => break,
            _ => {}
        }
    }
    Ok(())
}

/// Render a short result screen before returning to the main menu.
pub fn show_completion(term: &Term, title: &str, message: &str) -> Result<()> {
    render_header(term, title)?;
    println!("  {} {}", style("✓").green().bold(), style(message).green().bold());
    Ok(())
}

/// Ask whether an already installed optional component should be updated now.
pub fn confirm_component_update(component: &str) -> Result<bool> {
    Confirm::new()
        .with_prompt(format!("Update optional component '{}' now?", component))
        .default(false)
        .interact()
        .map_err(Into::into)
}

/// Interactive Main Menu.
pub fn show_main_menu(manifest: &Manifest, active_env: Option<&str>) -> Result<MainMenuAction> {
    let term = Term::stdout();
    let _ = term.hide_cursor();
    let res = main_menu_loop(&term, manifest, active_env);
    let _ = term.show_cursor();
    res
}

fn main_menu_loop(
    term: &Term,
    manifest: &Manifest,
    active_env: Option<&str>,
) -> Result<MainMenuAction> {
    let has_envs = manifest.environments.is_some();

    let mut entries: Vec<(&str, MainMenuAction)> = vec![
        ("📦 Manage Components", MainMenuAction::ManageComponents),
        ("⚡ Install Components", MainMenuAction::SyncSpecific),
        ("🗑️ Uninstall Components", MainMenuAction::UninstallSpecific),
    ];
    if has_envs {
        entries.push(("🖥️ Change Environment", MainMenuAction::ChangeEnvironment));
    }
    entries.extend_from_slice(&[
        ("🔄 Update Repository", MainMenuAction::UpdateRepo),
        ("📊 Status", MainMenuAction::Status),
        ("🚪 Exit", MainMenuAction::Exit),
    ]);

    let mut cursor = 0;

    loop {
        render_header(term, "Main Menu")?;

        if has_envs {
            let env_label = active_env.unwrap_or("none");
            println!("  Environment: {}\n", style(env_label).cyan().bold());
        }

        for (i, (label, _)) in entries.iter().enumerate() {
            let prefix = if i == cursor {
                style("❯").cyan().bold()
            } else {
                style(" ").dim()
            };
            let styled_label = if i == cursor {
                style(*label).cyan().bold()
            } else {
                style(*label)
            };
            println!("  {} {}", prefix, styled_label);
        }

        println!(
            "\n  {}",
            style("↑/↓: Navigate  •  Enter: Select  •  q/Esc: Exit").dim()
        );

        match term.read_key()? {
            Key::ArrowUp | Key::Char('k') => {
                cursor = if cursor == 0 { entries.len() - 1 } else { cursor - 1 };
            }
            Key::ArrowDown | Key::Char('j') => {
                cursor = (cursor + 1) % entries.len();
            }
            Key::Enter | Key::ArrowRight | Key::Char('l') => {
                return Ok(entries[cursor].1);
            }
            Key::Escape | Key::Char('q') => return Ok(MainMenuAction::Exit),
            _ => {}
        }
    }
}

/// Full declarative component management menu.
pub fn show_components_menu(
    components: &[&Component],
    state: &State,
) -> Result<Option<ComponentSelection>> {
    run_components_menu(components, state, SelectionMode::Declarative)
}

/// Targeted component sync/install menu (starts with nothing selected; does not uninstall unselected).
pub fn show_sync_specific_menu(
    components: &[&Component],
    state: &State,
) -> Result<Option<ComponentSelection>> {
    run_components_menu(components, state, SelectionMode::SyncSpecific)
}

/// Targeted component uninstall menu (starts with nothing selected; only uninstalls selected).
pub fn show_uninstall_menu(
    components: &[&Component],
    state: &State,
) -> Result<Option<ComponentSelection>> {
    run_components_menu(components, state, SelectionMode::UninstallSpecific)
}

fn run_components_menu(
    components: &[&Component],
    state: &State,
    mode: SelectionMode,
) -> Result<Option<ComponentSelection>> {
    if components.is_empty() {
        return Ok(None);
    }

    let term = Term::stdout();
    let _ = term.hide_cursor();
    let res = components_menu_loop(&term, components, state, mode);
    let _ = term.show_cursor();
    res
}

fn components_menu_loop(
    term: &Term,
    components: &[&Component],
    state: &State,
    mode: SelectionMode,
) -> Result<Option<ComponentSelection>> {
    let mut category_order: Vec<String> = Vec::new();
    let mut by_category: HashMap<String, Vec<&Component>> = HashMap::new();

    for comp in components {
        let cat = comp.category.as_deref().unwrap_or("general").to_string();
        if !by_category.contains_key(&cat) {
            category_order.push(cat.clone());
        }
        by_category.entry(cat).or_default().push(comp);
    }

    let mut selected_set: HashSet<String> = HashSet::new();
    if mode == SelectionMode::Declarative {
        for comp in components {
            let is_on = state
                .components
                .get(&comp.name)
                .copied()
                .unwrap_or(comp.default);
            if is_on {
                selected_set.insert(comp.name.clone());
            }
        }
    }

    let show_reinstall = mode != SelectionMode::UninstallSpecific;
    let mut reinstall = false;
    let mut cursor = 0;
    let action_rows = if show_reinstall { 2 } else { 1 };
    let total_rows = category_order.len() + action_rows;

    let header_title = match mode {
        SelectionMode::Declarative => "Manage Components",
        SelectionMode::SyncSpecific => "Install Components",
        SelectionMode::UninstallSpecific => "Uninstall Components",
    };

    loop {
        render_header(term, header_title)?;

        for (i, cat) in category_order.iter().enumerate() {
            let comps = &by_category[cat];
            let selected_count = comps
                .iter()
                .filter(|c| selected_set.contains(&c.name))
                .count();
            let total_count = comps.len();

            let is_selected = i == cursor;
            let pointer = if is_selected {
                style("❯").cyan().bold()
            } else {
                style(" ").dim()
            };

            let checkbox = match mode {
                SelectionMode::UninstallSpecific => {
                    if selected_count == total_count {
                        style("[✗]").red().bold()
                    } else if selected_count > 0 {
                        style("[~]").yellow().bold()
                    } else {
                        style("[ ]").dim()
                    }
                }
                _ => {
                    if selected_count == total_count {
                        style("[✓]").green().bold()
                    } else if selected_count > 0 {
                        style("[~]").yellow().bold()
                    } else {
                        style("[ ]").dim()
                    }
                }
            };

            let cat_name = if is_selected {
                style(format!("{:<15}", cat)).cyan().bold()
            } else {
                style(format!("{:<15}", cat))
            };

            let count_badge = style(format!("({}/{})", selected_count, total_count)).dim();

            println!("  {} {} 📁 {} {}", pointer, checkbox, cat_name, count_badge);
        }

        println!();

        let (reinstall_idx, save_idx) = if show_reinstall {
            let r_idx = category_order.len();
            let p_reinstall = if cursor == r_idx {
                style("❯").cyan().bold()
            } else {
                style(" ").dim()
            };
            let c_reinstall = if reinstall {
                style("[✓]").yellow().bold()
            } else {
                style("[ ]").dim()
            };
            let l_reinstall = if cursor == r_idx {
                style("Reinstall").yellow().bold()
            } else {
                style("Reinstall").yellow()
            };
            println!("  {} {} {}", p_reinstall, c_reinstall, l_reinstall);
            (Some(r_idx), category_order.len() + 1)
        } else {
            (None, category_order.len())
        };

        let p_save = if cursor == save_idx {
            style("❯").cyan().bold()
        } else {
            style(" ").dim()
        };
        let l_save = match mode {
            SelectionMode::Declarative => {
                if cursor == save_idx {
                    style("💾 Save & Apply").green().bold()
                } else {
                    style("💾 Save & Apply").green()
                }
            }
            SelectionMode::SyncSpecific => {
                if cursor == save_idx {
                    style("⚡ Install Selected").green().bold()
                } else {
                    style("⚡ Install Selected").green()
                }
            }
            SelectionMode::UninstallSpecific => {
                if cursor == save_idx {
                    style("🗑️ Uninstall Selected").red().bold()
                } else {
                    style("🗑️ Uninstall Selected").red()
                }
            }
        };
        println!("  {} {}", p_save, l_save);

        println!(
            "\n  {}",
            style("↑/↓: Navigate  •  Space: Toggle All  •  Enter: Open / Confirm  •  Esc: Cancel")
                .dim()
        );

        match term.read_key()? {
            Key::ArrowUp | Key::Char('k') => {
                cursor = if cursor == 0 { total_rows - 1 } else { cursor - 1 };
            }
            Key::ArrowDown | Key::Char('j') => {
                cursor = (cursor + 1) % total_rows;
            }
            Key::Char(' ') => {
                if cursor < category_order.len() {
                    let cat = &category_order[cursor];
                    let comps = &by_category[cat];
                    let all_selected = comps.iter().all(|c| selected_set.contains(&c.name));
                    if all_selected {
                        for comp in comps {
                            selected_set.remove(&comp.name);
                        }
                    } else {
                        for comp in comps {
                            selected_set.insert(comp.name.clone());
                        }
                    }
                } else if Some(cursor) == reinstall_idx {
                    reinstall = !reinstall;
                }
            }
            Key::Enter | Key::ArrowRight | Key::Char('l') => {
                if cursor < category_order.len() {
                    let cat = &category_order[cursor];
                    let comps = &by_category[cat];
                    category_components_loop(term, cat, comps, state, &mut selected_set, mode)?;
                } else if Some(cursor) == reinstall_idx {
                    reinstall = !reinstall;
                } else if cursor == save_idx {
                    match mode {
                        SelectionMode::Declarative => {
                            let mut enabled = Vec::new();
                            let mut disabled = Vec::new();
                            for comp in components {
                                if selected_set.contains(&comp.name) {
                                    enabled.push(comp.name.clone());
                                } else {
                                    disabled.push(comp.name.clone());
                                }
                            }
                            return Ok(Some(ComponentSelection {
                                enabled,
                                disabled,
                                reinstall,
                                skip_ask: false,
                            }));
                        }
                        SelectionMode::SyncSpecific => {
                            let enabled: Vec<String> = components
                                .iter()
                                .filter(|c| selected_set.contains(&c.name))
                                .map(|c| c.name.clone())
                                .collect();
                            if enabled.is_empty() {
                                return Ok(None);
                            }
                            return Ok(Some(ComponentSelection {
                                enabled,
                                disabled: Vec::new(),
                                reinstall,
                                skip_ask: true,
                            }));
                        }
                        SelectionMode::UninstallSpecific => {
                            let disabled: Vec<String> = components
                                .iter()
                                .filter(|c| selected_set.contains(&c.name))
                                .map(|c| c.name.clone())
                                .collect();
                            if disabled.is_empty() {
                                return Ok(None);
                            }
                            return Ok(Some(ComponentSelection {
                                enabled: Vec::new(),
                                disabled,
                                reinstall: false,
                                skip_ask: false,
                            }));
                        }
                    }
                } else {
                    return Ok(None);
                }
            }
            Key::Escape | Key::Char('q') => return Ok(None),
            _ => {}
        }
    }
}

/// Direct checkbox list for a specific category.
fn category_components_loop(
    term: &Term,
    category: &str,
    comps: &[&Component],
    state: &State,
    selected_set: &mut HashSet<String>,
    mode: SelectionMode,
) -> Result<()> {
    let mut cursor = 0;
    let total_rows = comps.len();

    loop {
        render_header(term, &format!("Category: {}", category))?;

        for (i, comp) in comps.iter().enumerate() {
            let is_selected = i == cursor;
            let is_checked = selected_set.contains(&comp.name);

            let pointer = if is_selected {
                style("❯").cyan().bold()
            } else {
                style(" ").dim()
            };

            let checkbox = match mode {
                SelectionMode::UninstallSpecific => {
                    if is_checked {
                        style("[✗]").red().bold()
                    } else {
                        style("[ ]").dim()
                    }
                }
                _ => {
                    if is_checked {
                        style("[✓]").green().bold()
                    } else {
                        style("[ ]").dim()
                    }
                }
            };

            let name = if is_selected {
                style(&comp.name).cyan().bold()
            } else {
                style(&comp.name)
            };

            let env_tag = comp
                .environment
                .as_ref()
                .map(|e| format!(" {}", style(format!("[{}]", e)).magenta()))
                .unwrap_or_default();

            let meta = style(format!(
                " ({} pkg, {} files)",
                comp.packages.len(),
                comp.files.len()
            ))
            .dim();

            let installed_badge = if mode == SelectionMode::SyncSpecific
                && (state.is_enabled(&comp.name) || state.deployed.contains_key(&comp.name))
            {
                format!(" {}", style("[installed]").green().dim())
            } else {
                String::new()
            };

            println!(
                "  {} {} {}{}{}{}",
                pointer, checkbox, name, env_tag, meta, installed_badge
            );
        }

        println!(
            "\n  {}",
            style("↑/↓: Navigate  •  Space: Toggle  •  a: Toggle All  •  Enter/Esc: Back").dim()
        );

        match term.read_key()? {
            Key::ArrowUp | Key::Char('k') => {
                cursor = if cursor == 0 { total_rows - 1 } else { cursor - 1 };
            }
            Key::ArrowDown | Key::Char('j') => {
                cursor = (cursor + 1) % total_rows;
            }
            Key::Char(' ') => {
                let name = &comps[cursor].name;
                if selected_set.contains(name) {
                    selected_set.remove(name);
                } else {
                    selected_set.insert(name.clone());
                }
            }
            Key::Char('a') => {
                let all_selected = comps.iter().all(|c| selected_set.contains(&c.name));
                if all_selected {
                    for comp in comps {
                        selected_set.remove(&comp.name);
                    }
                } else {
                    for comp in comps {
                        selected_set.insert(comp.name.clone());
                    }
                }
            }
            Key::Enter | Key::Escape | Key::ArrowLeft | Key::Char('h') | Key::Char('q') => {
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Interactive Environment Selection Menu.
pub fn show_environment_menu(
    manifest: &Manifest,
    current: Option<&str>,
) -> Result<Option<Option<String>>> {
    let envs = manifest.available_environments();
    if envs.is_empty() {
        println!("No environments configured in manifest.");
        return Ok(None);
    }

    let term = Term::stdout();
    let _ = term.hide_cursor();
    let res = environment_menu_loop(&term, &envs, current);
    let _ = term.show_cursor();
    res
}

fn environment_menu_loop(
    term: &Term,
    envs: &[&str],
    current: Option<&str>,
) -> Result<Option<Option<String>>> {
    let mut cursor = match current {
        None => 0,
        Some(c) => envs
            .iter()
            .position(|e| *e == c)
            .map(|idx| idx + 1)
            .unwrap_or(0),
    };

    let total_rows = envs.len() + 1;

    loop {
        render_header(term, "Select Environment")?;

        let current_label = current.unwrap_or("none");
        println!("  Current: {}\n", style(current_label).cyan().bold());

        for i in 0..total_rows {
            let is_selected = i == cursor;
            let pointer = if is_selected {
                style("❯").cyan().bold()
            } else {
                style(" ").dim()
            };

            let (name, is_active) = if i == 0 {
                ("none", current.is_none())
            } else {
                let env_name = envs[i - 1];
                (env_name, current == Some(env_name))
            };

            let styled_name = if is_selected {
                style(name).cyan().bold()
            } else {
                style(name)
            };

            let active_badge = if is_active {
                format!(" {}", style("(active)").green())
            } else {
                String::new()
            };

            println!("  {} {}{}", pointer, styled_name, active_badge);
        }

        println!(
            "\n  {}",
            style("↑/↓: Navigate  •  Enter: Select  •  Esc: Cancel").dim()
        );

        match term.read_key()? {
            Key::ArrowUp | Key::Char('k') => {
                cursor = if cursor == 0 { total_rows - 1 } else { cursor - 1 };
            }
            Key::ArrowDown | Key::Char('j') => {
                cursor = (cursor + 1) % total_rows;
            }
            Key::Enter => {
                if cursor == 0 {
                    return Ok(Some(None));
                } else {
                    return Ok(Some(Some(envs[cursor - 1].to_string())));
                }
            }
            Key::Escape | Key::Char('q') => return Ok(None),
            _ => {}
        }
    }
}

fn status_lines(manifest: &Manifest, state: &State) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(ref url) = state.repo_url {
        lines.push(format!("  Repository: {}", style(url).dim()));
    }
    let env_label = state
        .active_environment
        .as_deref()
        .unwrap_or("none");
    lines.push(format!(
        "  Active environment: {}",
        style(env_label).cyan().bold()
    ));

    let active_env = state.active_environment.as_deref();
    let components = manifest.components_for_environment(active_env);

    if components.is_empty() {
        lines.push("  No components configured.".to_string());
        return lines;
    }

    let mut category_order: Vec<String> = Vec::new();
    let mut by_category: HashMap<String, Vec<&Component>> = HashMap::new();

    for comp in &components {
        let cat = comp.category.as_deref().unwrap_or("general").to_string();
        if !by_category.contains_key(&cat) {
            category_order.push(cat.clone());
        }
        by_category.entry(cat).or_default().push(comp);
    }

    lines.push(String::new());
    for cat in &category_order {
        lines.push(format!("  {}", style(format!("┌─ {} ─┐", cat)).dim()));
        for comp in &by_category[cat] {
            let enabled = state.is_enabled(&comp.name);
            let status_icon = if enabled {
                style("✓").green().bold()
            } else {
                style("✗").red()
            };
            let deployed_count = state
                .deployed
                .get(&comp.name)
                .map(|d| d.len())
                .unwrap_or(0);

            let env_tag = comp
                .environment
                .as_ref()
                .map(|e| format!(" [{}]", e))
                .unwrap_or_default();

            if enabled && deployed_count > 0 {
                lines.push(format!(
                    "    {} {}{} {} files deployed",
                    status_icon,
                    comp.name,
                    style(&env_tag).dim(),
                    style(format!("({})", deployed_count)).dim()
                ));
            } else {
                lines.push(format!(
                    "    {} {}{}",
                    status_icon,
                    comp.name,
                    style(&env_tag).dim()
                ));
            }
        }
    }
    lines
}

/// Print a status report for the non-interactive CLI command.
pub fn show_status(manifest: &Manifest, state: &State) {
    for line in status_lines(manifest, state) {
        println!("{}", line);
    }
}

/// Show a scrollable status view in the full-screen menu.
pub fn show_status_menu(manifest: &Manifest, state: &State) -> Result<()> {
    let term = Term::stdout();
    let lines = status_lines(manifest, state);
    let mut offset = 0usize;
    let _ = term.hide_cursor();

    let result = (|| -> Result<()> {
        loop {
            render_header(&term, "System Status")?;
            let (rows, _) = term.size();
            let visible_rows = usize::from(rows).saturating_sub(7).max(1);
            let max_offset = lines.len().saturating_sub(visible_rows);
            offset = offset.min(max_offset);

            for line in lines.iter().skip(offset).take(visible_rows) {
                println!("{}", line);
            }

            let position = if max_offset == 0 {
                String::new()
            } else {
                format!("  •  {}/{}", offset + 1, max_offset + 1)
            };
            println!(
                "\n  {}{}",
                style("↑/↓: Scroll  •  Enter/Esc: Back").dim(),
                style(position).dim()
            );

            match term.read_key()? {
                Key::ArrowUp | Key::Char('k') => offset = offset.saturating_sub(1),
                Key::ArrowDown | Key::Char('j') => offset = (offset + 1).min(max_offset),
                Key::Enter | Key::Escape | Key::ArrowLeft | Key::Char('h') | Key::Char('q') => {
                    break
                }
                _ => {}
            }
        }
        Ok(())
    })();

    let _ = term.show_cursor();
    result
}