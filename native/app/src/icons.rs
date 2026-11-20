//! Bundled lucide icons (ISC — see `icons/LICENSE.txt`). Glyphs render via
//! GPUI's `svg()` element which inherits `text_color`, matching lucide's
//! `stroke="currentColor"`.

use gpui::{Styled, Svg, px, svg};

#[allow(dead_code)] // Consumed per-surface as icons land.
/// Every bundled lucide glyph.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Icon {
    /// `activity`
    Activity,
    /// `archive`
    Archive,
    /// `arrow-left`
    ArrowLeft,
    /// `arrow-up`
    ArrowUp,
    /// `at-sign`
    AtSign,
    /// `bell`
    Bell,
    /// `bookmark`
    Bookmark,
    /// `check`
    Check,
    /// `chevron-down`
    ChevronDown,
    /// `chevron-left`
    ChevronLeft,
    /// `chevron-right`
    ChevronRight,
    /// `chevron-up`
    ChevronUp,
    /// `chevrons-up-down`
    ChevronsUpDown,
    /// `circle`
    Circle,
    /// `circle-alert`
    CircleAlert,
    /// `circle-dot`
    CircleDot,
    /// `circle-help`
    CircleHelp,
    /// `clock`
    Clock,
    /// `copy`
    Copy,
    /// `corner-down-left`
    CornerDownLeft,
    /// `dot`
    Dot,
    /// `download`
    Download,
    /// `ellipsis`
    Ellipsis,
    /// `external-link`
    ExternalLink,
    /// `eye`
    Eye,
    /// `file`
    File,
    /// `file-code`
    FileCode,
    /// `file-text`
    FileText,
    /// `folder`
    Folder,
    /// `folder-git-2`
    FolderGit2,
    /// `folder-open`
    FolderOpen,
    /// `git-branch`
    GitBranch,
    /// `git-compare`
    GitCompare,
    /// `git-merge`
    GitMerge,
    /// `git-pull-request`
    GitPullRequest,
    /// `globe`
    Globe,
    /// `image`
    Image,
    /// `info`
    Info,
    /// `keyboard`
    Keyboard,
    /// `link`
    Link,
    /// `list-todo`
    ListTodo,
    /// `loader-circle`
    LoaderCircle,
    /// `lock`
    Lock,
    /// `maximize-2`
    Maximize2,
    /// `message-square`
    MessageSquare,
    /// `mic`
    Mic,
    /// `minus`
    Minus,
    /// `monitor`
    Monitor,
    /// `moon`
    Moon,
    /// `panel-bottom`
    PanelBottom,
    /// `panel-right`
    PanelRight,
    /// `pause`
    Pause,
    /// `pencil`
    Pencil,
    /// `pin`
    Pin,
    /// `play`
    Play,
    /// `plus`
    Plus,
    /// `quote`
    Quote,
    /// `refresh-cw`
    RefreshCw,
    /// `scroll-text`
    ScrollText,
    /// `search`
    Search,
    /// `settings`
    Settings,
    /// `shield`
    Shield,
    /// `smartphone`
    Smartphone,
    /// `sparkles`
    Sparkles,
    /// `square`
    Square,
    /// `star`
    Star,
    /// `tag`
    Tag,
    /// `terminal`
    Terminal,
    /// `thumbs-down`
    ThumbsDown,
    /// `thumbs-up`
    ThumbsUp,
    /// `trash-2`
    Trash2,
    /// `triangle-alert`
    TriangleAlert,
    /// `unlink`
    Unlink,
    /// `upload`
    Upload,
    /// `users`
    Users,
    /// `wrench`
    Wrench,
    /// `x`
    X,
    /// `zap`
    Zap,
}

#[allow(dead_code)]
impl Icon {
    /// Raw SVG bytes for embedding via `svg().data(...)`.
    pub fn data(self) -> &'static [u8] {
        match self {
            Icon::Activity => include_bytes!("../icons/activity.svg").as_slice(),
            Icon::Archive => include_bytes!("../icons/archive.svg").as_slice(),
            Icon::ArrowLeft => include_bytes!("../icons/arrow-left.svg").as_slice(),
            Icon::ArrowUp => include_bytes!("../icons/arrow-up.svg").as_slice(),
            Icon::AtSign => include_bytes!("../icons/at-sign.svg").as_slice(),
            Icon::Bell => include_bytes!("../icons/bell.svg").as_slice(),
            Icon::Bookmark => include_bytes!("../icons/bookmark.svg").as_slice(),
            Icon::Check => include_bytes!("../icons/check.svg").as_slice(),
            Icon::ChevronDown => include_bytes!("../icons/chevron-down.svg").as_slice(),
            Icon::ChevronLeft => include_bytes!("../icons/chevron-left.svg").as_slice(),
            Icon::ChevronRight => include_bytes!("../icons/chevron-right.svg").as_slice(),
            Icon::ChevronUp => include_bytes!("../icons/chevron-up.svg").as_slice(),
            Icon::ChevronsUpDown => include_bytes!("../icons/chevrons-up-down.svg").as_slice(),
            Icon::Circle => include_bytes!("../icons/circle.svg").as_slice(),
            Icon::CircleAlert => include_bytes!("../icons/circle-alert.svg").as_slice(),
            Icon::CircleDot => include_bytes!("../icons/circle-dot.svg").as_slice(),
            Icon::CircleHelp => include_bytes!("../icons/circle-help.svg").as_slice(),
            Icon::Clock => include_bytes!("../icons/clock.svg").as_slice(),
            Icon::Copy => include_bytes!("../icons/copy.svg").as_slice(),
            Icon::CornerDownLeft => include_bytes!("../icons/corner-down-left.svg").as_slice(),
            Icon::Dot => include_bytes!("../icons/dot.svg").as_slice(),
            Icon::Download => include_bytes!("../icons/download.svg").as_slice(),
            Icon::Ellipsis => include_bytes!("../icons/ellipsis.svg").as_slice(),
            Icon::ExternalLink => include_bytes!("../icons/external-link.svg").as_slice(),
            Icon::Eye => include_bytes!("../icons/eye.svg").as_slice(),
            Icon::File => include_bytes!("../icons/file.svg").as_slice(),
            Icon::FileCode => include_bytes!("../icons/file-code.svg").as_slice(),
            Icon::FileText => include_bytes!("../icons/file-text.svg").as_slice(),
            Icon::Folder => include_bytes!("../icons/folder.svg").as_slice(),
            Icon::FolderGit2 => include_bytes!("../icons/folder-git-2.svg").as_slice(),
            Icon::FolderOpen => include_bytes!("../icons/folder-open.svg").as_slice(),
            Icon::GitBranch => include_bytes!("../icons/git-branch.svg").as_slice(),
            Icon::GitCompare => include_bytes!("../icons/git-compare.svg").as_slice(),
            Icon::GitMerge => include_bytes!("../icons/git-merge.svg").as_slice(),
            Icon::GitPullRequest => include_bytes!("../icons/git-pull-request.svg").as_slice(),
            Icon::Globe => include_bytes!("../icons/globe.svg").as_slice(),
            Icon::Image => include_bytes!("../icons/image.svg").as_slice(),
            Icon::Info => include_bytes!("../icons/info.svg").as_slice(),
            Icon::Keyboard => include_bytes!("../icons/keyboard.svg").as_slice(),
            Icon::Link => include_bytes!("../icons/link.svg").as_slice(),
            Icon::ListTodo => include_bytes!("../icons/list-todo.svg").as_slice(),
            Icon::LoaderCircle => include_bytes!("../icons/loader-circle.svg").as_slice(),
            Icon::Lock => include_bytes!("../icons/lock.svg").as_slice(),
            Icon::Maximize2 => include_bytes!("../icons/maximize-2.svg").as_slice(),
            Icon::MessageSquare => include_bytes!("../icons/message-square.svg").as_slice(),
            Icon::Mic => include_bytes!("../icons/mic.svg").as_slice(),
            Icon::Minus => include_bytes!("../icons/minus.svg").as_slice(),
            Icon::Monitor => include_bytes!("../icons/monitor.svg").as_slice(),
            Icon::Moon => include_bytes!("../icons/moon.svg").as_slice(),
            Icon::PanelBottom => include_bytes!("../icons/panel-bottom.svg").as_slice(),
            Icon::PanelRight => include_bytes!("../icons/panel-right.svg").as_slice(),
            Icon::Pause => include_bytes!("../icons/pause.svg").as_slice(),
            Icon::Pencil => include_bytes!("../icons/pencil.svg").as_slice(),
            Icon::Pin => include_bytes!("../icons/pin.svg").as_slice(),
            Icon::Play => include_bytes!("../icons/play.svg").as_slice(),
            Icon::Plus => include_bytes!("../icons/plus.svg").as_slice(),
            Icon::Quote => include_bytes!("../icons/quote.svg").as_slice(),
            Icon::RefreshCw => include_bytes!("../icons/refresh-cw.svg").as_slice(),
            Icon::ScrollText => include_bytes!("../icons/scroll-text.svg").as_slice(),
            Icon::Search => include_bytes!("../icons/search.svg").as_slice(),
            Icon::Settings => include_bytes!("../icons/settings.svg").as_slice(),
            Icon::Shield => include_bytes!("../icons/shield.svg").as_slice(),
            Icon::Smartphone => include_bytes!("../icons/smartphone.svg").as_slice(),
            Icon::Sparkles => include_bytes!("../icons/sparkles.svg").as_slice(),
            Icon::Square => include_bytes!("../icons/square.svg").as_slice(),
            Icon::Star => include_bytes!("../icons/star.svg").as_slice(),
            Icon::Tag => include_bytes!("../icons/tag.svg").as_slice(),
            Icon::Terminal => include_bytes!("../icons/terminal.svg").as_slice(),
            Icon::ThumbsDown => include_bytes!("../icons/thumbs-down.svg").as_slice(),
            Icon::ThumbsUp => include_bytes!("../icons/thumbs-up.svg").as_slice(),
            Icon::Trash2 => include_bytes!("../icons/trash-2.svg").as_slice(),
            Icon::TriangleAlert => include_bytes!("../icons/triangle-alert.svg").as_slice(),
            Icon::Unlink => include_bytes!("../icons/unlink.svg").as_slice(),
            Icon::Upload => include_bytes!("../icons/upload.svg").as_slice(),
            Icon::Users => include_bytes!("../icons/users.svg").as_slice(),
            Icon::Wrench => include_bytes!("../icons/wrench.svg").as_slice(),
            Icon::X => include_bytes!("../icons/x.svg").as_slice(),
            Icon::Zap => include_bytes!("../icons/zap.svg").as_slice(),
        }
    }

    /// Lucide kebab-case name (for diagnostics).
    pub fn name(self) -> &'static str {
        match self {
            Icon::Activity => "activity",
            Icon::Archive => "archive",
            Icon::ArrowLeft => "arrow-left",
            Icon::ArrowUp => "arrow-up",
            Icon::AtSign => "at-sign",
            Icon::Bell => "bell",
            Icon::Bookmark => "bookmark",
            Icon::Check => "check",
            Icon::ChevronDown => "chevron-down",
            Icon::ChevronLeft => "chevron-left",
            Icon::ChevronRight => "chevron-right",
            Icon::ChevronUp => "chevron-up",
            Icon::ChevronsUpDown => "chevrons-up-down",
            Icon::Circle => "circle",
            Icon::CircleAlert => "circle-alert",
            Icon::CircleDot => "circle-dot",
            Icon::CircleHelp => "circle-help",
            Icon::Clock => "clock",
            Icon::Copy => "copy",
            Icon::CornerDownLeft => "corner-down-left",
            Icon::Dot => "dot",
            Icon::Download => "download",
            Icon::Ellipsis => "ellipsis",
            Icon::ExternalLink => "external-link",
            Icon::Eye => "eye",
            Icon::File => "file",
            Icon::FileCode => "file-code",
            Icon::FileText => "file-text",
            Icon::Folder => "folder",
            Icon::FolderGit2 => "folder-git-2",
            Icon::FolderOpen => "folder-open",
            Icon::GitBranch => "git-branch",
            Icon::GitCompare => "git-compare",
            Icon::GitMerge => "git-merge",
            Icon::GitPullRequest => "git-pull-request",
            Icon::Globe => "globe",
            Icon::Image => "image",
            Icon::Info => "info",
            Icon::Keyboard => "keyboard",
            Icon::Link => "link",
            Icon::ListTodo => "list-todo",
            Icon::LoaderCircle => "loader-circle",
            Icon::Lock => "lock",
            Icon::Maximize2 => "maximize-2",
            Icon::MessageSquare => "message-square",
            Icon::Mic => "mic",
            Icon::Minus => "minus",
            Icon::Monitor => "monitor",
            Icon::Moon => "moon",
            Icon::PanelBottom => "panel-bottom",
            Icon::PanelRight => "panel-right",
            Icon::Pause => "pause",
            Icon::Pencil => "pencil",
            Icon::Pin => "pin",
            Icon::Play => "play",
            Icon::Plus => "plus",
            Icon::Quote => "quote",
            Icon::RefreshCw => "refresh-cw",
            Icon::ScrollText => "scroll-text",
            Icon::Search => "search",
            Icon::Settings => "settings",
            Icon::Shield => "shield",
            Icon::Smartphone => "smartphone",
            Icon::Sparkles => "sparkles",
            Icon::Square => "square",
            Icon::Star => "star",
            Icon::Tag => "tag",
            Icon::Terminal => "terminal",
            Icon::ThumbsDown => "thumbs-down",
            Icon::ThumbsUp => "thumbs-up",
            Icon::Trash2 => "trash-2",
            Icon::TriangleAlert => "triangle-alert",
            Icon::Unlink => "unlink",
            Icon::Upload => "upload",
            Icon::Users => "users",
            Icon::Wrench => "wrench",
            Icon::X => "x",
            Icon::Zap => "zap",
        }
    }
}

/// Render `icon` at `size` px. Color comes from `.text_color(...)` on the
/// returned element — lucide strokes `currentColor`.
#[allow(dead_code)] // Consumed per-surface as icons land.
pub fn icon(icon: Icon, size: f32) -> Svg {
    svg().data(icon.data()).w(px(size)).h(px(size))
}
