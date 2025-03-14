use crossterm::{
    cursor::Show,
    event::{self, Event as CEvent, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use git2::Repository;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};
use std::{
    collections::HashMap,
    error::Error,
    fs, io,
    path::{Path, PathBuf},
};
use tokio::sync::mpsc;

#[derive(Debug)]
struct FileNode {
    name: String,
    path: PathBuf,
    is_dir: bool,
    rect: Rect,
    color: Color,
    children: Vec<FileNode>,
    loaded: bool, // Indicates if children have been loaded
}

impl FileNode {
    fn new(name: String, path: PathBuf, is_dir: bool) -> Self {
        // Files are always "loaded"; for directories, we delay loading.
        FileNode {
            name,
            path,
            is_dir,
            rect: Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            color: Color::White,
            children: Vec::new(),
            loaded: !is_dir,
        }
    }
}

// Return a node for the given path without recursing.
fn build_file_tree(path: &PathBuf) -> io::Result<FileNode> {
    let metadata = fs::metadata(path)?;
    let is_dir = metadata.is_dir();
    let name = if let Some(n) = path.file_name() {
        n.to_string_lossy().to_string()
    } else {
        path.to_string_lossy().to_string()
    };
    Ok(FileNode::new(name, path.to_path_buf(), is_dir))
}

// Lazily load children for a directory node.
fn load_children(node: &mut FileNode) -> io::Result<()> {
    if !node.is_dir || node.loaded {
        return Ok(());
    }
    let mut children = Vec::new();
    if let Ok(entries) = fs::read_dir(&node.path) {
        for entry in entries.flatten() {
            let child_path = entry.path();
            let metadata = fs::metadata(&child_path)?;
            let is_dir = metadata.is_dir();
            let name = if let Some(n) = child_path.file_name() {
                n.to_string_lossy().to_string()
            } else {
                child_path.to_string_lossy().to_string()
            };
            children.push(FileNode::new(name, child_path, is_dir));
        }
    }
    node.children = children;
    Ok(())
}

// Recursively load directories up to the given depth.
fn lazy_load_to_depth(node: &mut FileNode, depth: usize) {
    if depth == 0 {
        return;
    }
    if node.is_dir {
        _ = load_children(node);
        for child in &mut node.children {
            lazy_load_to_depth(child, depth - 1);
        }
    }
}

// (Layout and drawing functions remain unchanged.)
fn inner_rect(rect: Rect) -> Rect {
    Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    }
}

fn layout_nodes(nodes: &mut [FileNode], area: Rect, depth: u8) {
    if nodes.is_empty() {
        return;
    }
    let mut columns: Vec<Vec<usize>> = Vec::new();
    let mut current_col = Vec::new();
    let mut current_sum = 0u16;
    for (i, node) in nodes.iter().enumerate() {
        let base = if node.is_dir {
            std::cmp::max(3, 1 + node.children.len() as u16)
        } else {
            1
        };
        if !current_col.is_empty() && current_sum + base > area.height {
            columns.push(current_col);
            current_col = Vec::new();
            current_sum = 0;
        }
        current_col.push(i);
        current_sum += base;
    }
    if !current_col.is_empty() {
        columns.push(current_col);
    }
    let num_cols = columns.len() as u16;
    if num_cols == 0 {
        return;
    }
    let col_width = area.width / num_cols;
    for (col_index, col) in columns.iter().enumerate() {
        let total_base: u16 = col
            .iter()
            .map(|&i| {
                if nodes[i].is_dir {
                    std::cmp::max(3, 1 + nodes[i].children.len() as u16)
                } else {
                    1
                }
            })
            .sum();
        let mut y = area.y;
        if total_base <= area.height {
            let extra_space = area.height - total_base;
            let extra_per_item = if !col.is_empty() {
                extra_space / col.len() as u16
            } else {
                0
            };
            for &i in col.iter() {
                let base = if nodes[i].is_dir {
                    std::cmp::max(3, 1 + nodes[i].children.len() as u16)
                } else {
                    1
                };
                let height = base + extra_per_item;
                nodes[i].rect = Rect {
                    x: area.x + col_index as u16 * col_width,
                    y,
                    width: col_width,
                    height,
                };
                y += height;
                if nodes[i].is_dir && !nodes[i].children.is_empty() && depth < 2 {
                    layout_nodes(&mut nodes[i].children, inner_rect(nodes[i].rect), depth + 1);
                }
            }
        } else {
            let scale = area.height as f32 / total_base as f32;
            for &i in col.iter() {
                let base = if nodes[i].is_dir {
                    std::cmp::max(3, 1 + nodes[i].children.len() as u16)
                } else {
                    1
                };
                let height = ((base as f32) * scale).max(1.0).floor() as u16;
                let height = if y + height > area.y + area.height {
                    (area.y + area.height).saturating_sub(y)
                } else {
                    height
                };
                nodes[i].rect = Rect {
                    x: area.x + col_index as u16 * col_width,
                    y,
                    width: col_width,
                    height,
                };
                y += height;
                if nodes[i].is_dir && !nodes[i].children.is_empty() && depth < 2 {
                    layout_nodes(&mut nodes[i].children, inner_rect(nodes[i].rect), depth + 1);
                }
            }
        }
    }
}

fn layout_directory(node: &mut FileNode, area: Rect) {
    node.rect = area;
    let content_area = inner_rect(area);
    layout_nodes(&mut node.children, content_area, 0);
}

fn draw_file_node(
    f: &mut ratatui::Frame,
    node: &FileNode,
    current_dir_path: &Path,
    selected_child: Option<&PathBuf>,
    highlight_depth: u8,
    current_depth: usize,
) {
    if current_depth >= 3 {
        return;
    }
    let is_highlight =
        highlight_depth > 0 || (selected_child.is_some() && node.path == *selected_child.unwrap());
    if node.is_dir {
        let mut block = Block::default()
            .borders(Borders::ALL)
            .title(Line::from(node.name.clone()));
        if is_highlight {
            block = block.border_style(Style::default().fg(Color::Magenta));
        }
        f.render_widget(block, node.rect);
        let child_highlight = if is_highlight { 1 } else { 0 };
        for child in &node.children {
            draw_file_node(
                f,
                child,
                current_dir_path,
                None,
                child_highlight,
                current_depth + 1,
            );
        }
    } else {
        let style = if is_highlight {
            Style::default().fg(Color::Magenta)
        } else {
            Style::default().fg(node.color)
        };
        let text = format!("◉ {}", node.name);
        let paragraph = Paragraph::new(text).style(style);
        f.render_widget(paragraph, node.rect);
    }
}

fn draw_current_view(
    f: &mut ratatui::Frame,
    node: &FileNode,
    current_dir_path: &Path,
    selected_child: Option<&PathBuf>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(node.name.clone()));
    f.render_widget(block, node.rect);
    for child in node.children.iter() {
        let child_highlight = if let Some(sel) = selected_child {
            if child.path == *sel { 1 } else { 0 }
        } else {
            0
        };
        draw_file_node(
            f,
            child,
            current_dir_path,
            selected_child,
            child_highlight,
            0,
        );
    }
}

fn get_current_node<'a>(node: &'a FileNode, path_stack: &[usize]) -> &'a FileNode {
    let mut cur = node;
    for &i in path_stack {
        if i < cur.children.len() {
            cur = &cur.children[i];
        }
    }
    cur
}

fn get_current_node_mut<'a>(node: &'a mut FileNode, path_stack: &[usize]) -> &'a mut FileNode {
    let mut cur = node;
    for &i in path_stack {
        cur = &mut cur.children[i];
    }
    cur
}

fn update_tree_status(
    node: &mut FileNode,
    repo: &Repository,
    status_map: &HashMap<String, git2::Status>,
) {
    if let Some(workdir) = repo.workdir() {
        if let Ok(relative_path) = node.path.strip_prefix(workdir) {
            let rel_str = relative_path.to_string_lossy().to_string();
            if let Some(status) = status_map.get(&rel_str) {
                node.color = color_for_git_status(Some(*status));
            }
        }
    }
    for child in &mut node.children {
        update_tree_status(child, repo, status_map);
    }
}

fn color_for_git_status(status: Option<git2::Status>) -> Color {
    if let Some(s) = status {
        if s.is_index_new() || s.is_wt_new() {
            Color::Green
        } else if s.is_index_modified() || s.is_wt_modified() {
            Color::Yellow
        } else if s.is_index_deleted() || s.is_wt_deleted() {
            Color::Red
        } else {
            Color::White
        }
    } else {
        Color::White
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _guard = TerminalGuard;
    let args: Vec<String> = std::env::args().collect();
    let root_path_str = if args.len() > 1 {
        args[1].clone()
    } else {
        ".".to_string()
    };
    let root_path = PathBuf::from(root_path_str);

    // Build only the root node.
    let mut file_tree = build_file_tree(&root_path)?;
    let mut path_stack: Vec<usize> = Vec::new();
    let mut selection_stack: Vec<usize> = vec![0];
    lazy_load_to_depth(&mut file_tree, 3);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let repo = Repository::discover(root_path.clone()).ok();

    // Status map update (using tokio as before).
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let Some(repo) = Repository::discover(root_path).ok() else {
            return;
        };
        let map = tokio::task::spawn_blocking(move || build_status_map(&repo))
            .await
            .expect("spawn_blocking failed");
        let _ = tx.send(map);
    });

    loop {
        if let Ok(status_map) = rx.try_recv() {
            if let Some(ref repo) = repo {
                update_tree_status(&mut file_tree, repo, &status_map);
            }
        }

        let full_area = terminal.size()?;
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(2), Constraint::Length(3)].as_ref())
                .split(Rect {
                    x: 0,
                    y: 0,
                    width: full_area.width,
                    height: full_area.height,
                });
            {
                let area = chunks[0];
                let current_node_mut = get_current_node_mut(&mut file_tree, &path_stack);
                layout_directory(current_node_mut, area);
            }

            let current_node = get_current_node(&file_tree, &path_stack);
            let current_dir_path = &current_node.path;
            let selected_child = if !current_node.children.is_empty() {
                let sel = *selection_stack.last().unwrap();
                Some(&current_node.children[sel].path)
            } else {
                None
            };

            let selected_path = selected_child.unwrap_or(current_dir_path);
            let relative_path = selected_path
                .strip_prefix(&file_tree.path)
                .unwrap_or(selected_path);
            let metadata = fs::metadata(selected_path).ok();
            let file_type = metadata
                .as_ref()
                .map(|m| if m.is_dir() { "Directory" } else { "File" })
                .unwrap_or("Unknown");
            let permissions = metadata
                .as_ref()
                .map(|m| {
                    if m.permissions().readonly() {
                        "Read-only"
                    } else {
                        "Writable"
                    }
                })
                .unwrap_or("Unknown");
            let size_info = metadata
                .as_ref()
                .and_then(|m| {
                    if !m.is_dir() {
                        Some(format!(" {} bytes", m.len()))
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            let status_text = format!(
                "Name: {} | Type: {} | Permissions: {}{}",
                relative_path.display(),
                file_type,
                permissions,
                size_info
            );
            let status_bar = Paragraph::new(status_text)
                .block(Block::default().borders(Borders::ALL).title("Info"))
                .style(Style::default().fg(Color::White));
            draw_current_view(f, current_node, current_dir_path, selected_child);
            f.render_widget(status_bar, chunks[1]);
        })?;

        if let CEvent::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Up => {
                    if let Some(sel) = selection_stack.last_mut() {
                        if *sel > 0 {
                            *sel -= 1;
                        }
                    }
                }
                KeyCode::Down => {
                    let current_node = get_current_node(&file_tree, &path_stack);
                    if let Some(sel) = selection_stack.last_mut() {
                        if *sel < current_node.children.len().saturating_sub(1) {
                            *sel += 1;
                        }
                    }
                }
                KeyCode::Right => {
                    // Get mutable reference so we can load children on demand.
                    let current_node_mut = get_current_node_mut(&mut file_tree, &path_stack);
                    if !current_node_mut.children.is_empty() {
                        let sel = *selection_stack.last().unwrap();
                        if current_node_mut.children[sel].is_dir
                            && !current_node_mut.children[sel].loaded
                        {
                            lazy_load_to_depth(&mut current_node_mut.children[sel], 3)
                        }
                        path_stack.push(sel);
                        selection_stack.push(0);
                    }
                }
                KeyCode::Left => {
                    if !path_stack.is_empty() {
                        path_stack.pop();
                        selection_stack.pop();
                    }
                }
                KeyCode::Enter => {
                    let current_node = get_current_node(&file_tree, &path_stack);
                    let selected_path = if !current_node.children.is_empty() {
                        let sel = *selection_stack.last().unwrap();
                        &current_node.children[sel].path
                    } else {
                        &current_node.path
                    };
                    // Exit TUI mode.
                    disable_raw_mode()?;
                    execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;
                    println!("{}", selected_path.display());
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn build_status_map(repo: &Repository) -> HashMap<String, git2::Status> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true);
    let statuses = repo
        .statuses(Some(&mut opts))
        .expect("Failed to get statuses");
    let mut map = HashMap::new();
    for entry in statuses.iter() {
        if let Some(path) = entry.path() {
            map.insert(path.to_string(), entry.status());
        }
    }
    map
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, Show);
    }
}
