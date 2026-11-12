use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;

use crate::profile::{Device, Profile};
use anyhow::Result;
use console::{style, Key, Style, Term};
use dialoguer::{Confirm, Input};

use crate::manifest::{Component, Manifest};
use crate::state::State;

#[derive(Clone, Copy)]
pub enum MainMenuAction {
    ManageComponents,
    InstallSpecific,
    DeployDotfiles,
    CaptureChanges,
    ReinstallPackages,
    UninstallSpecific,
    ChangeEnvironment,
    ChangeDevice,
    Check,
    UpdateRepo,
    Status,
    Exit,
}

pub struct ComponentSelection {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
    pub skip_ask: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectionMode {
    /// Full declarative state: pre-selects enabled/default; unchecked items are uninstalled.
    Declarative,
    /// Targeted sync/install: starts empty; only selected items are synced, nothing is uninstalled.
    InstallSpecific,
    /// Targeted uninstall: starts empty; only selected items are uninstalled, nothing is synced.
    UninstallSpecific,
    ReinstallPackages,
}

/// Clear the visible screen and print a header without erasing scrollback.
pub fn render_header(term: &Term, subtitle: &str) -> Result<()> {
    print!("\x1b[2J\x1b[H");
    term.flush()?;
    println!(
        "  {} {}",
        style("◆ waysync").cyan().bold(),
        style(format!("── {}", subtitle)).bold()
    );
    println!(
        "  {}\n",
        style("─".repeat(usize::from(term.size().1).saturating_sub(4).clamp(8, 56)))
            .cyan()
            .dim()
    );
    Ok(())
}

/// Wait for the user to press Enter/Space/Esc before clearing the screen again.
pub fn wait_for_enter() -> Result<()> {
    let term = Term::stdout();
    println!("\n  {}", style("Enter to return").dim());
    loop {
        match term.read_key()? {
            Key::Enter | Key::Escape | Key::Char(' ') | Key::Char('q') => break,
            _ => {}
        }
    }
    Ok(())
}

/// Ask whether an already installed optional component should be updated now.
pub fn confirm_component_update(component: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        println!("  Skipping optional file update for '{component}' (no interactive terminal).");
        return Ok(false);
    }
    Confirm::new()
        .with_prompt(format!("Update '{}'?", component))
        .default(false)
        .interact()
        .map_err(Into::into)
}

#[derive(Clone, Copy, Default)]
enum MenuPage {
    #[default]
    Home,
    Components,
    Dotfiles,
    Profile,
    Repository,
}

#[derive(Default)]
pub struct Navigation {
    page: MenuPage,
    cursor: usize,
    home_cursor: usize,
}

enum MenuEntry {
    Group(&'static str, MenuPage),
    Action(&'static str, MainMenuAction),
    Back,
}

impl MenuEntry {
    fn icon(&self) -> &str {
        match self {
            Self::Group(_, MenuPage::Components) => "📦",
            Self::Group(_, MenuPage::Dotfiles) => "📄",
            Self::Group(_, MenuPage::Profile) => "🎨",
            Self::Group(_, MenuPage::Repository) => "🌿",
            Self::Group(_, MenuPage::Home) => "⌂",
            Self::Action(_, action) => match action {
                MainMenuAction::ManageComponents => "☑",
                MainMenuAction::InstallSpecific => "📥",
                MainMenuAction::UninstallSpecific => "🗑",
                MainMenuAction::ReinstallPackages => "🔧",
                MainMenuAction::DeployDotfiles => "📤",
                MainMenuAction::CaptureChanges => "📸",
                MainMenuAction::ChangeEnvironment => "🎨",
                MainMenuAction::ChangeDevice => "💻",
                MainMenuAction::UpdateRepo => "🔄",
                MainMenuAction::Check => "🔎",
                MainMenuAction::Status => "📊",
                MainMenuAction::Exit => "🚪",
            },
            Self::Back => "↩",
        }
    }

    fn accent(&self) -> Style {
        match self {
            Self::Group(_, MenuPage::Components)
            | Self::Action(_, MainMenuAction::ManageComponents) => Style::new().cyan(),
            Self::Group(_, MenuPage::Dotfiles)
            | Self::Action(_, MainMenuAction::DeployDotfiles | MainMenuAction::CaptureChanges) => {
                Style::new().blue()
            }
            Self::Group(_, MenuPage::Profile)
            | Self::Action(_, MainMenuAction::ChangeEnvironment | MainMenuAction::ChangeDevice) => {
                Style::new().magenta()
            }
            Self::Group(_, MenuPage::Repository)
            | Self::Action(_, MainMenuAction::UpdateRepo | MainMenuAction::ReinstallPackages) => {
                Style::new().yellow()
            }
            Self::Action(
                _,
                MainMenuAction::InstallSpecific | MainMenuAction::Check | MainMenuAction::Status,
            ) => Style::new().green(),
            Self::Action(_, MainMenuAction::UninstallSpecific | MainMenuAction::Exit) => {
                Style::new().red()
            }
            Self::Back | Self::Group(_, MenuPage::Home) => Style::new().dim(),
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Group(label, _) | Self::Action(label, _) => label,
            Self::Back => "Back",
        }
    }
}

fn page_entries(page: MenuPage, has_environments: bool) -> Vec<MenuEntry> {
    use MainMenuAction::*;
    use MenuEntry::{Action, Back, Group};
    match page {
        MenuPage::Home => vec![
            Group("Components ›", MenuPage::Components),
            Group("Dotfiles ›", MenuPage::Dotfiles),
            Group("Profile ›", MenuPage::Profile),
            Group("Repository ›", MenuPage::Repository),
            Action("Status", Status),
            Action("Quit", Exit),
        ],
        MenuPage::Components => vec![
            Action("Select & apply", ManageComponents),
            Action("Install", InstallSpecific),
            Action("Uninstall", UninstallSpecific),
            Action("Reinstall packages", ReinstallPackages),
            Back,
        ],
        MenuPage::Dotfiles => vec![
            Action("Deploy", DeployDotfiles),
            Action("Capture edits", CaptureChanges),
            Back,
        ],
        MenuPage::Profile => {
            let mut entries = Vec::new();
            if has_environments {
                entries.push(Action("Environment", ChangeEnvironment));
            }
            entries.extend([Action("Device", ChangeDevice), Back]);
            entries
        }
        MenuPage::Repository => vec![
            Action("Pull changes", UpdateRepo),
            Action("Check", Check),
            Back,
        ],
    }
}

pub fn show_main_menu(
    manifest: &Manifest,
    profile: Profile<'_>,
    navigation: &mut Navigation,
) -> Result<MainMenuAction> {
    let term = Term::stdout();
    let _ = term.hide_cursor();
    let result = main_menu_loop(&term, manifest, profile, navigation);
    let _ = term.show_cursor();
    result
}

fn main_menu_loop(
    term: &Term,
    manifest: &Manifest,
    profile: Profile<'_>,
    nav: &mut Navigation,
) -> Result<MainMenuAction> {
    loop {
        let entries = page_entries(nav.page, !manifest.available_environments().is_empty());
        nav.cursor = nav.cursor.min(entries.len() - 1);
        let home = matches!(nav.page, MenuPage::Home);
        let title = match nav.page {
            MenuPage::Home => "Main Menu",
            MenuPage::Components => "Components",
            MenuPage::Dotfiles => "Dotfiles",
            MenuPage::Profile => "Profile",
            MenuPage::Repository => "Repository",
        };
        render_header(term, title)?;
        let device_icon = match profile.device {
            Some(Device::Desktop) => "🖥",
            Some(Device::Laptop) => "💻",
            None => "◌",
        };
        println!(
            "  🎨 {}   {} {}\n",
            style(profile.environment.unwrap_or("none"))
                .magenta()
                .bold(),
            device_icon,
            style(
                profile
                    .device
                    .map(|device| device.to_string())
                    .unwrap_or_else(|| "none".into())
            )
            .blue()
            .bold(),
        );
        let range = visible_range(entries.len(), nav.cursor, term.size().0, 8);
        for (index, entry) in entries.iter().enumerate().take(range.end).skip(range.start) {
            let icon_padding =
                " ".repeat(2usize.saturating_sub(console::measure_text_width(entry.icon())));
            let label = format!(" {}{} {} ", entry.icon(), icon_padding, entry.label());
            if index == nav.cursor {
                println!(
                    "  {} {}",
                    entry.accent().bold().apply_to("▸"),
                    entry.accent().bold().reverse().apply_to(label)
                );
            } else {
                println!("    {}", entry.accent().apply_to(label));
            }
        }
        println!(
            "\n  {} {}  ·  {} {}  ·  {} {}",
            style("↑/↓").cyan(),
            style("Move").dim(),
            style("Enter").cyan(),
            style("Select").dim(),
            style("Esc").cyan(),
            style(if home { "Quit" } else { "Back" }).dim(),
        );
        match term.read_key()? {
            Key::ArrowUp | Key::Char('k') => {
                nav.cursor = if nav.cursor == 0 {
                    entries.len() - 1
                } else {
                    nav.cursor - 1
                }
            }
            Key::ArrowDown | Key::Char('j') => nav.cursor = (nav.cursor + 1) % entries.len(),
            Key::Enter | Key::ArrowRight | Key::Char('l') => match entries[nav.cursor] {
                MenuEntry::Action(_, action) => return Ok(action),
                MenuEntry::Group(_, page) => {
                    nav.home_cursor = nav.cursor;
                    nav.page = page;
                    nav.cursor = 0;
                }
                MenuEntry::Back => {
                    nav.page = MenuPage::Home;
                    nav.cursor = nav.home_cursor;
                }
            },
            Key::Escape | Key::ArrowLeft | Key::Char('h') | Key::Char('q') => {
                if home {
                    return Ok(MainMenuAction::Exit);
                }
                nav.page = MenuPage::Home;
                nav.cursor = nav.home_cursor;
            }
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
pub fn show_install_menu(
    components: &[&Component],
    state: &State,
) -> Result<Option<ComponentSelection>> {
    run_components_menu(components, state, SelectionMode::InstallSpecific)
}

/// Targeted component uninstall menu (starts with nothing selected; only uninstalls selected).
pub fn show_uninstall_menu(
    components: &[&Component],
    state: &State,
) -> Result<Option<ComponentSelection>> {
    run_components_menu(components, state, SelectionMode::UninstallSpecific)
}

pub fn show_reinstall_menu(
    components: &[&Component],
    state: &State,
) -> Result<Option<ComponentSelection>> {
    run_components_menu(components, state, SelectionMode::ReinstallPackages)
}

pub fn capture_checkout(current: Option<&std::path::Path>) -> Result<std::path::PathBuf> {
    render_header(&Term::stdout(), "Capture Local Changes")?;
    let input = Input::<String>::new()
        .with_prompt("Dotfiles checkout")
        .with_initial_text(
            current
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
        .interact_text()?;
    Ok(std::path::PathBuf::from(crate::utils::expand_env(&input)))
}

pub fn confirm_deploy_hooks() -> Result<bool> {
    Confirm::new()
        .with_prompt("Run deployment hooks?")
        .default(true)
        .interact()
        .map_err(Into::into)
}

fn run_components_menu(
    components: &[&Component],
    state: &State,
    mode: SelectionMode,
) -> Result<Option<ComponentSelection>> {
    if components.is_empty() {
        println!("No components are available for this action.");
        wait_for_enter()?;
        return Ok(None);
    }

    let term = Term::stdout();
    let _ = term.hide_cursor();
    let res = components_menu_loop(&term, components, state, mode);
    let _ = term.show_cursor();
    let Some(selection) = res? else {
        return Ok(None);
    };
    let removals: Vec<_> = selection
        .disabled
        .iter()
        .filter(|name| state.is_installed(name))
        .collect();
    term.clear_screen()?;
    match mode {
        SelectionMode::Declarative => {
            let installs = selection
                .enabled
                .iter()
                .filter(|name| !state.is_installed(name))
                .count();
            println!(
                "  Install {}  ·  Deploy {}  ·  Remove {}",
                installs,
                selection.enabled.len() - installs,
                removals.len()
            );
        }
        SelectionMode::InstallSpecific => {
            println!("  Install {} components", selection.enabled.len())
        }
        SelectionMode::UninstallSpecific => println!("  Uninstall {} components", removals.len()),
        SelectionMode::ReinstallPackages => println!(
            "  Reinstall packages: {} components",
            selection.enabled.len()
        ),
    }
    if !removals.is_empty() {
        println!(
            "  Remove: {}",
            removals
                .iter()
                .map(|name| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if Confirm::new()
        .with_prompt("Apply?")
        .default(removals.is_empty())
        .interact()?
    {
        Ok(Some(selection))
    } else {
        Ok(None)
    }
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

    let mut cursor = 0;
    let total_rows = category_order.len() + 1;

    let header_title = match mode {
        SelectionMode::Declarative => "Manage Components",
        SelectionMode::InstallSpecific => "Install Components",
        SelectionMode::UninstallSpecific => "Uninstall Components",
        SelectionMode::ReinstallPackages => "Reinstall Packages Only",
    };

    loop {
        render_header(term, header_title)?;

        let range = visible_range(
            category_order.len(),
            cursor.min(category_order.len().saturating_sub(1)),
            term.size().0,
            12,
        );
        println!(
            "  Categories {}–{} of {}",
            range.start + 1,
            range.end,
            category_order.len()
        );
        for (i, cat) in category_order
            .iter()
            .enumerate()
            .take(range.end)
            .skip(range.start)
        {
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

        let save_idx = category_order.len();
        let p_save = if cursor == save_idx {
            style("❯").cyan().bold()
        } else {
            style(" ").dim()
        };
        let l_save = match mode {
            SelectionMode::Declarative => {
                if cursor == save_idx {
                    style("💾 Apply Selection").green().bold()
                } else {
                    style("💾 Apply Selection").green()
                }
            }
            SelectionMode::InstallSpecific => {
                if cursor == save_idx {
                    style("⚡ Install Selected").green().bold()
                } else {
                    style("⚡ Install Selected").green()
                }
            }
            SelectionMode::ReinstallPackages => {
                if cursor == save_idx {
                    style("🔧 Reinstall Selected Packages").yellow().bold()
                } else {
                    style("🔧 Reinstall Selected Packages").yellow()
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
                cursor = if cursor == 0 {
                    total_rows - 1
                } else {
                    cursor - 1
                };
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
                }
            }
            Key::Enter | Key::ArrowRight | Key::Char('l') => {
                if cursor < category_order.len() {
                    let cat = &category_order[cursor];
                    let comps = &by_category[cat];
                    category_components_loop(term, cat, comps, state, &mut selected_set, mode)?;
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
                                skip_ask: false,
                            }));
                        }
                        SelectionMode::InstallSpecific | SelectionMode::ReinstallPackages => {
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
    let mut query = String::new();

    loop {
        let visible: Vec<_> = comps
            .iter()
            .copied()
            .filter(|component| {
                component
                    .name
                    .to_lowercase()
                    .contains(&query.to_lowercase())
            })
            .collect();
        let total_rows = visible.len();
        cursor = cursor.min(total_rows.saturating_sub(1));
        render_header(term, &format!("Category: {}", category))?;
        let range = visible_range(total_rows, cursor, term.size().0, 10);
        println!(
            "  {} matching components  •  Filter: {}\n",
            total_rows,
            if query.is_empty() { "all" } else { &query }
        );

        for (i, comp) in visible.iter().enumerate().take(range.end).skip(range.start) {
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
                .environment_label()
                .map(|environments| format!(" {}", style(format!("[{}]", environments)).magenta()))
                .unwrap_or_default();

            let meta = style(format!(
                " ({} pkg, {} files)",
                comp.packages.len(),
                comp.files.len()
            ))
            .dim();

            let installed_badge = if state.is_installed(&comp.name) {
                format!(" {}", style("[managed]").green().dim())
            } else {
                String::new()
            };

            let device_tag = if comp.devices.is_empty() {
                String::new()
            } else {
                format!(
                    " [{}]",
                    comp.devices
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            println!(
                "  {} {} {}{}{}{}{}",
                pointer, checkbox, name, env_tag, device_tag, meta, installed_badge
            );
        }

        println!(
            "\n  {}",
            style("↑/↓: Navigate  •  Space: Toggle  •  a: Toggle Matches  •  /: Filter  •  Enter/Esc: Back").dim()
        );

        match term.read_key()? {
            Key::ArrowUp | Key::Char('k') => {
                cursor = if cursor == 0 {
                    total_rows.saturating_sub(1)
                } else {
                    cursor - 1
                };
            }
            Key::ArrowDown | Key::Char('j') => {
                cursor = (cursor + 1) % total_rows.max(1);
            }
            Key::Char(' ') if total_rows > 0 => {
                let name = &visible[cursor].name;
                if selected_set.contains(name) {
                    selected_set.remove(name);
                } else {
                    selected_set.insert(name.clone());
                }
            }
            Key::Char('/') => {
                let _ = term.show_cursor();
                query = Input::<String>::new()
                    .with_prompt("Filter (empty clears)")
                    .allow_empty(true)
                    .interact_text()?;
                let _ = term.hide_cursor();
                cursor = 0;
            }
            Key::Char('a') => {
                let all_selected = visible.iter().all(|c| selected_set.contains(&c.name));
                if all_selected {
                    for comp in &visible {
                        selected_set.remove(&comp.name);
                    }
                } else {
                    for comp in &visible {
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
    let res = environment_menu_loop(&term, &envs, current, "Select Environment");
    let _ = term.show_cursor();
    res
}

fn environment_menu_loop(
    term: &Term,
    envs: &[&str],
    current: Option<&str>,
    title: &str,
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
        render_header(term, title)?;

        let current_label = current.unwrap_or("none");
        println!("  Current: {}\n", style(current_label).cyan().bold());

        for i in visible_range(total_rows, cursor, term.size().0, 9) {
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
                cursor = if cursor == 0 {
                    total_rows - 1
                } else {
                    cursor - 1
                };
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
    let env_label = state.active_environment.as_deref().unwrap_or("none");
    lines.push(format!(
        "  Active environment: {}",
        style(env_label).cyan().bold()
    ));

    lines.push(format!(
        "  Device: {}",
        style(
            state
                .active_device
                .map(|device| device.to_string())
                .unwrap_or_else(|| "none".into())
        )
        .cyan()
        .bold()
    ));
    lines.push(
        "  Installed status is tracked by Waysync; external packages are not scanned.".into(),
    );
    let components = manifest.components_for_profile(state.profile());
    for component in &manifest.components {
        if !component.applies_to(state.profile()) && state.is_installed(&component.name) {
            lines.push(format!(
                "  ! {}: inactive but still installed; cleanup pending",
                component.name
            ));
        }
    }

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
            let installed = state.is_installed(&comp.name);
            let status_icon = if enabled {
                style("✓").green().bold()
            } else {
                style("✗").red()
            };
            let deployed_count = state.deployed.get(&comp.name).map(|d| d.len()).unwrap_or(0);

            if state.pending_install.contains(&comp.name) {
                lines.push(format!(
                    "    ! {} (installation hook pending; run sync)",
                    comp.name
                ));
            }
            let env_tag = comp
                .environment_label()
                .map(|environments| format!(" [{}]", environments))
                .unwrap_or_default();

            if !enabled && installed {
                lines.push(format!("    ! {} (disabled; cleanup pending)", comp.name));
            } else if enabled && !installed {
                lines.push(format!("    ! {} (enabled; sync pending)", comp.name));
            } else if enabled && deployed_count > 0 {
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

/// Keep the current row visible, including on short laptop terminals.
fn visible_range(
    total: usize,
    cursor: usize,
    rows: u16,
    reserved: usize,
) -> std::ops::Range<usize> {
    let height = usize::from(rows).saturating_sub(reserved).max(1);
    let start = cursor
        .saturating_sub(height / 2)
        .min(total.saturating_sub(height));
    start..(start + height).min(total)
}

pub fn show_device_menu(current: Option<Device>) -> Result<Option<Option<Device>>> {
    let term = Term::stdout();
    let label = current.map(|device| device.to_string());
    let _ = term.hide_cursor();
    let result = environment_menu_loop(
        &term,
        &["desktop", "laptop"],
        label.as_deref(),
        "Select Device",
    );
    let _ = term.show_cursor();
    Ok(result?.map(|choice| {
        choice.map(|name| {
            if name == "desktop" {
                Device::Desktop
            } else {
                Device::Laptop
            }
        })
    }))
}

pub struct ScreenGuard;
impl ScreenGuard {
    pub fn enter(term: &Term) -> Result<Self> {
        print!("\x1b[?1049h");
        term.flush()?;
        Ok(Self)
    }
}
impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let term = Term::stdout();
        let _ = term.show_cursor();
        print!("\x1b[?1049l");
        let _ = term.flush();
    }
}

/// Preserve command output in scrollback and keep recoverable errors in the app.
pub fn with_visible_output(term: &Term, operation: impl FnOnce() -> Result<()>) -> Result<()> {
    print!("\x1b[?1049l");
    let _ = term.show_cursor();
    term.flush()?;
    match operation() {
        Ok(()) => {}
        Err(error) => eprintln!("\nError: {error:#}"),
    }
    wait_for_enter()?;
    print!("\x1b[?1049h");
    term.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_keeps_cursor_visible_at_both_ends_and_on_short_terminals() {
        for total in [0, 1, 5, 80] {
            for rows in [6, 24, 50] {
                for cursor in 0..total {
                    let range = visible_range(total, cursor, rows, 10);
                    assert!(range.contains(&cursor));
                    assert!(range.end <= total);
                }
            }
        }
    }
}
