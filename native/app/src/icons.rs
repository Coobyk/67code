//! Bundled lucide icons (ISC — see `icons/LICENSE.txt`). Glyphs render via
//! GPUI's `svg()` element which inherits `text_color`, matching lucide's
//! `stroke="currentColor"`.

use gpui::{Hsla, Styled, Svg, prelude::*, px, svg};

#[allow(dead_code)] // Consumed per-surface as icons land.
/// Every bundled lucide glyph.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Icon {
    /// `activity`
    Activity,
    /// `alarm-clock`
    AlarmClock,
    /// `archive`
    Archive,
    /// `arrow-left`
    ArrowLeft,
    /// `app-window`
    AppWindow,
    /// `arrow-down`
    ArrowDown,
    /// `arrow-up`
    ArrowUp,
    /// `at-sign`
    AtSign,
    /// `bell`
    Bell,
    /// `bookmark`
    Bookmark,
    /// `book-plus`
    BookPlus,
    /// `bot`
    Bot,
    /// `bug`
    Bug,
    /// `calendar-clock`
    CalendarClock,
    /// `chart-no-axes-column`
    ChartNoAxesColumn,
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
    /// `chevrons-down-up`
    ChevronsDownUp,
    /// `chevrons-up-down`
    ChevronsUpDown,
    /// `circle`
    Circle,
    /// `circle-stop`
    CircleStop,
    /// `circle-alert`
    CircleAlert,
    /// `circle-check`
    CircleCheck,
    /// `circle-dashed`
    CircleDashed,
    /// `circle-dot`
    CircleDot,
    /// `circle-dot-dashed`
    CircleDotDashed,
    /// `circle-help`
    CircleHelp,
    /// `clock`
    Clock,
    /// `cloud-upload`
    CloudUpload,
    /// `cloud-download`
    CloudDownload,
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
    /// `file-diff`
    FileDiff,
    /// `file-search`
    FileSearch,
    /// `file-text`
    FileText,
    /// `flask-conical`
    FlaskConical,
    /// `folder`
    Folder,
    /// `folder-git-2`
    FolderGit2,
    /// `folder-open`
    FolderOpen,
    /// `folder-plus`
    FolderPlus,
    /// `folder-tree`
    FolderTree,
    /// `git-branch`
    GitBranch,
    /// `git-commit-horizontal`
    GitCommitHorizontal,
    /// `git-compare`
    GitCompare,
    /// `git-fork`
    GitFork,
    /// `git-merge`
    GitMerge,
    /// `git-pull-request`
    GitPullRequest,
    /// `globe`
    Globe,
    /// `hash`
    Hash,
    /// `image`
    Image,
    /// `info`
    Info,
    /// `keyboard`
    Keyboard,
    /// `key-round`
    KeyRound,
    /// `layers`
    Layers,
    /// `link`
    Link,
    /// `list-filter`
    ListFilter,
    /// `list-todo`
    ListTodo,
    /// `loader-circle`
    LoaderCircle,
    /// `lock`
    Lock,
    /// `maximize-2`
    Maximize2,
    /// `mail-open`
    MailOpen,
    /// `message-square`
    MessageSquare,
    /// `message-circle-question`
    MessageCircleQuestion,
    /// `message-square-text`
    MessageSquareText,
    /// `messages-square`
    MessagesSquare,
    /// `message-square-plus`
    MessageSquarePlus,
    /// `message-square-dashed`
    MessageSquareDashed,
    /// `mic`
    Mic,
    /// `minus`
    Minus,
    /// `monitor`
    Monitor,
    /// `moon`
    Moon,
    /// `palette`
    Palette,
    /// `panel-bottom`
    PanelBottom,
    /// `paperclip`
    Paperclip,
    /// `panel-right`
    PanelRight,
    /// `panels-top-left`
    PanelsTopLeft,
    /// `pause`
    Pause,
    /// `pen-line`
    PenLine,
    /// `pencil`
    Pencil,
    /// `pin`
    Pin,
    /// `pin-off`
    PinOff,
    /// `play`
    Play,
    /// `plug`
    Plug,
    /// `plus`
    Plus,
    /// `provider-claude` — Claude star (brand mark from apps/web Icons.tsx).
    ProviderClaude,
    /// `provider-openai` — OpenAI knot (brand mark from apps/web Icons.tsx).
    ProviderOpenAi,
    /// `quote`
    Quote,
    /// `refresh-cw`
    RefreshCw,
    /// `scroll-text`
    ScrollText,
    /// `search`
    Search,
    /// `server`
    Server,
    /// `settings`
    Settings,
    /// `settings-2`
    Settings2,
    /// `shield`
    Shield,
    /// `shield-check`
    ShieldCheck,
    /// `shield-off`
    ShieldOff,
    /// `shield-question`
    ShieldQuestion,
    /// `smartphone`
    Smartphone,
    /// `snap-shot` — the web's custom SnapShot capture glyph (capture-frame
    /// corners + window rect + lens circle, from `SettingsSidebarNav.tsx`).
    SnapShot,
    /// `sparkles`
    Sparkles,
    /// `square`
    Square,
    /// `square-arrow-out-up-right`
    SquareArrowOutUpRight,
    /// `square-dashed`
    SquareDashed,
    /// `square-menu`
    SquareMenu,
    /// `square-pen`
    SquarePen,
    /// `square-split-horizontal`
    SquareSplitHorizontal,
    /// `square-split-vertical`
    SquareSplitVertical,
    /// `square-terminal`
    SquareTerminal,
    /// `star`
    Star,
    /// `star` filled — favourites rail button and favourited model rows.
    StarFilled,
    /// `tag`
    Tag,
    /// `terminal`
    Terminal,
    /// `thumbs-down`
    ThumbsDown,
    /// `thumbs-up`
    ThumbsUp,
    /// `timer`
    Timer,
    /// `trash-2`
    Trash2,
    /// `triangle-alert`
    TriangleAlert,
    /// `undo-2`
    Undo2,
    /// `unlink`
    Unlink,
    /// `upload`
    Upload,
    /// `users`
    Users,
    /// `user-round`
    UserRound,
    /// `user-plus`
    UserPlus,
    /// `workflow`
    Workflow,
    /// `wrap-text`
    WrapText,
    /// `wrench`
    Wrench,
    /// `x`
    X,
    /// `zap`
    Zap,
    /// `zed` — Zed editor brand mark (apps/web Icons.tsx `Zed`).
    Zed,
}

#[allow(dead_code)]
impl Icon {
    /// Raw SVG bytes for embedding via `svg().data(...)`.
    pub fn data(self) -> &'static [u8] {
        match self {
            Icon::Activity => include_bytes!("../icons/activity.svg").as_slice(),
            Icon::AlarmClock => include_bytes!("../icons/alarm-clock.svg").as_slice(),
            Icon::AppWindow => include_bytes!("../icons/app-window.svg").as_slice(),
            Icon::Archive => include_bytes!("../icons/archive.svg").as_slice(),
            Icon::ArrowDown => include_bytes!("../icons/arrow-down.svg").as_slice(),
            Icon::ArrowLeft => include_bytes!("../icons/arrow-left.svg").as_slice(),
            Icon::ArrowUp => include_bytes!("../icons/arrow-up.svg").as_slice(),
            Icon::AtSign => include_bytes!("../icons/at-sign.svg").as_slice(),
            Icon::Bell => include_bytes!("../icons/bell.svg").as_slice(),
            Icon::Bookmark => include_bytes!("../icons/bookmark.svg").as_slice(),
            Icon::BookPlus => include_bytes!("../icons/book-plus.svg").as_slice(),
            Icon::Bot => include_bytes!("../icons/bot.svg").as_slice(),
            Icon::Bug => include_bytes!("../icons/bug.svg").as_slice(),
            Icon::CalendarClock => include_bytes!("../icons/calendar-clock.svg").as_slice(),
            Icon::ChartNoAxesColumn => {
                include_bytes!("../icons/chart-no-axes-column.svg").as_slice()
            }
            Icon::Check => include_bytes!("../icons/check.svg").as_slice(),
            Icon::ChevronDown => include_bytes!("../icons/chevron-down.svg").as_slice(),
            Icon::ChevronLeft => include_bytes!("../icons/chevron-left.svg").as_slice(),
            Icon::ChevronRight => include_bytes!("../icons/chevron-right.svg").as_slice(),
            Icon::ChevronUp => include_bytes!("../icons/chevron-up.svg").as_slice(),
            Icon::ChevronsDownUp => include_bytes!("../icons/chevrons-down-up.svg").as_slice(),
            Icon::ChevronsUpDown => include_bytes!("../icons/chevrons-up-down.svg").as_slice(),
            Icon::Circle => include_bytes!("../icons/circle.svg").as_slice(),
            Icon::CircleAlert => include_bytes!("../icons/circle-alert.svg").as_slice(),
            Icon::CircleCheck => include_bytes!("../icons/circle-check.svg").as_slice(),
            Icon::CircleDashed => include_bytes!("../icons/circle-dashed.svg").as_slice(),
            Icon::CircleDot => include_bytes!("../icons/circle-dot.svg").as_slice(),
            Icon::CircleDotDashed => include_bytes!("../icons/circle-dot-dashed.svg").as_slice(),
            Icon::CircleHelp => include_bytes!("../icons/circle-help.svg").as_slice(),
            Icon::CircleStop => include_bytes!("../icons/circle-stop.svg").as_slice(),
            Icon::Clock => include_bytes!("../icons/clock.svg").as_slice(),
            Icon::CloudUpload => include_bytes!("../icons/cloud-upload.svg").as_slice(),
            Icon::CloudDownload => include_bytes!("../icons/cloud-download.svg").as_slice(),
            Icon::Copy => include_bytes!("../icons/copy.svg").as_slice(),
            Icon::CornerDownLeft => include_bytes!("../icons/corner-down-left.svg").as_slice(),
            Icon::Dot => include_bytes!("../icons/dot.svg").as_slice(),
            Icon::Download => include_bytes!("../icons/download.svg").as_slice(),
            Icon::Ellipsis => include_bytes!("../icons/ellipsis.svg").as_slice(),
            Icon::ExternalLink => include_bytes!("../icons/external-link.svg").as_slice(),
            Icon::Eye => include_bytes!("../icons/eye.svg").as_slice(),
            Icon::File => include_bytes!("../icons/file.svg").as_slice(),
            Icon::FileCode => include_bytes!("../icons/file-code.svg").as_slice(),
            Icon::FileDiff => include_bytes!("../icons/file-diff.svg").as_slice(),
            Icon::FileSearch => include_bytes!("../icons/file-search.svg").as_slice(),
            Icon::FileText => include_bytes!("../icons/file-text.svg").as_slice(),
            Icon::FlaskConical => include_bytes!("../icons/flask-conical.svg").as_slice(),
            Icon::Folder => include_bytes!("../icons/folder.svg").as_slice(),
            Icon::FolderGit2 => include_bytes!("../icons/folder-git-2.svg").as_slice(),
            Icon::FolderOpen => include_bytes!("../icons/folder-open.svg").as_slice(),
            Icon::FolderPlus => include_bytes!("../icons/folder-plus.svg").as_slice(),
            Icon::FolderTree => include_bytes!("../icons/folder-tree.svg").as_slice(),
            Icon::GitBranch => include_bytes!("../icons/git-branch.svg").as_slice(),
            Icon::GitCommitHorizontal => {
                include_bytes!("../icons/git-commit-horizontal.svg").as_slice()
            }
            Icon::GitCompare => include_bytes!("../icons/git-compare.svg").as_slice(),
            Icon::GitFork => include_bytes!("../icons/git-fork.svg").as_slice(),
            Icon::GitMerge => include_bytes!("../icons/git-merge.svg").as_slice(),
            Icon::GitPullRequest => include_bytes!("../icons/git-pull-request.svg").as_slice(),
            Icon::Globe => include_bytes!("../icons/globe.svg").as_slice(),
            Icon::Hash => include_bytes!("../icons/hash.svg").as_slice(),
            Icon::Image => include_bytes!("../icons/image.svg").as_slice(),
            Icon::Info => include_bytes!("../icons/info.svg").as_slice(),
            Icon::KeyRound => include_bytes!("../icons/key-round.svg").as_slice(),
            Icon::Layers => include_bytes!("../icons/layers.svg").as_slice(),
            Icon::Keyboard => include_bytes!("../icons/keyboard.svg").as_slice(),
            Icon::Link => include_bytes!("../icons/link.svg").as_slice(),
            Icon::ListFilter => include_bytes!("../icons/list-filter.svg").as_slice(),
            Icon::ListTodo => include_bytes!("../icons/list-todo.svg").as_slice(),
            Icon::LoaderCircle => include_bytes!("../icons/loader-circle.svg").as_slice(),
            Icon::Lock => include_bytes!("../icons/lock.svg").as_slice(),
            Icon::Maximize2 => include_bytes!("../icons/maximize-2.svg").as_slice(),
            Icon::MailOpen => include_bytes!("../icons/mail-open.svg").as_slice(),
            Icon::MessageSquare => include_bytes!("../icons/message-square.svg").as_slice(),
            Icon::MessageCircleQuestion => include_bytes!("../icons/message-circle-question.svg").as_slice(),
            Icon::MessageSquareText => {
                include_bytes!("../icons/message-square-text.svg").as_slice()
            }
            Icon::MessagesSquare => include_bytes!("../icons/messages-square.svg").as_slice(),
            Icon::MessageSquarePlus => include_bytes!("../icons/message-square-plus.svg").as_slice(),
            Icon::MessageSquareDashed => include_bytes!("../icons/message-square-dashed.svg").as_slice(),
            Icon::Mic => include_bytes!("../icons/mic.svg").as_slice(),
            Icon::Minus => include_bytes!("../icons/minus.svg").as_slice(),
            Icon::Monitor => include_bytes!("../icons/monitor.svg").as_slice(),
            Icon::Moon => include_bytes!("../icons/moon.svg").as_slice(),
            Icon::Palette => include_bytes!("../icons/palette.svg").as_slice(),
            Icon::PanelBottom => include_bytes!("../icons/panel-bottom.svg").as_slice(),
            Icon::Paperclip => include_bytes!("../icons/paperclip.svg").as_slice(),
            Icon::PanelRight => include_bytes!("../icons/panel-right.svg").as_slice(),
            Icon::PanelsTopLeft => include_bytes!("../icons/panels-top-left.svg").as_slice(),
            Icon::Pause => include_bytes!("../icons/pause.svg").as_slice(),
            Icon::PenLine => include_bytes!("../icons/pen-line.svg").as_slice(),
            Icon::Pencil => include_bytes!("../icons/pencil.svg").as_slice(),
            Icon::Pin => include_bytes!("../icons/pin.svg").as_slice(),
            Icon::PinOff => include_bytes!("../icons/pin-off.svg").as_slice(),
            Icon::Play => include_bytes!("../icons/play.svg").as_slice(),
            Icon::Plug => include_bytes!("../icons/plug.svg").as_slice(),
            Icon::Plus => include_bytes!("../icons/plus.svg").as_slice(),
            Icon::ProviderClaude => include_bytes!("../icons/provider-claude.svg").as_slice(),
            Icon::ProviderOpenAi => include_bytes!("../icons/provider-openai.svg").as_slice(),
            Icon::Quote => include_bytes!("../icons/quote.svg").as_slice(),
            Icon::RefreshCw => include_bytes!("../icons/refresh-cw.svg").as_slice(),
            Icon::ScrollText => include_bytes!("../icons/scroll-text.svg").as_slice(),
            Icon::Search => include_bytes!("../icons/search.svg").as_slice(),
            Icon::Server => include_bytes!("../icons/server.svg").as_slice(),
            Icon::Settings => include_bytes!("../icons/settings.svg").as_slice(),
            Icon::Settings2 => include_bytes!("../icons/settings-2.svg").as_slice(),
            Icon::Shield => include_bytes!("../icons/shield.svg").as_slice(),
            Icon::ShieldCheck => include_bytes!("../icons/shield-check.svg").as_slice(),
            Icon::ShieldOff => include_bytes!("../icons/shield-off.svg").as_slice(),
            Icon::ShieldQuestion => include_bytes!("../icons/shield-question.svg").as_slice(),
            Icon::Smartphone => include_bytes!("../icons/smartphone.svg").as_slice(),
            Icon::SnapShot => include_bytes!("../icons/snap-shot.svg").as_slice(),
            Icon::Sparkles => include_bytes!("../icons/sparkles.svg").as_slice(),
            Icon::Square => include_bytes!("../icons/square.svg").as_slice(),
            Icon::SquareArrowOutUpRight => {
                include_bytes!("../icons/square-arrow-out-up-right.svg").as_slice()
            }
            Icon::SquareDashed => include_bytes!("../icons/square-dashed.svg").as_slice(),
            Icon::SquareMenu => include_bytes!("../icons/square-menu.svg").as_slice(),
            Icon::SquarePen => include_bytes!("../icons/square-pen.svg").as_slice(),
            Icon::SquareSplitHorizontal => {
                include_bytes!("../icons/square-split-horizontal.svg").as_slice()
            }
            Icon::SquareSplitVertical => {
                include_bytes!("../icons/square-split-vertical.svg").as_slice()
            }
            Icon::SquareTerminal => include_bytes!("../icons/square-terminal.svg").as_slice(),
            Icon::Star => include_bytes!("../icons/star.svg").as_slice(),
            Icon::StarFilled => include_bytes!("../icons/star-filled.svg").as_slice(),
            Icon::Tag => include_bytes!("../icons/tag.svg").as_slice(),
            Icon::Terminal => include_bytes!("../icons/terminal.svg").as_slice(),
            Icon::ThumbsDown => include_bytes!("../icons/thumbs-down.svg").as_slice(),
            Icon::ThumbsUp => include_bytes!("../icons/thumbs-up.svg").as_slice(),
            Icon::Timer => include_bytes!("../icons/timer.svg").as_slice(),
            Icon::Trash2 => include_bytes!("../icons/trash-2.svg").as_slice(),
            Icon::TriangleAlert => include_bytes!("../icons/triangle-alert.svg").as_slice(),
            Icon::Undo2 => include_bytes!("../icons/undo-2.svg").as_slice(),
            Icon::Unlink => include_bytes!("../icons/unlink.svg").as_slice(),
            Icon::Upload => include_bytes!("../icons/upload.svg").as_slice(),
            Icon::Users => include_bytes!("../icons/users.svg").as_slice(),
            Icon::UserRound => include_bytes!("../icons/user-round.svg").as_slice(),
            Icon::UserPlus => include_bytes!("../icons/user-plus.svg").as_slice(),
            Icon::Workflow => include_bytes!("../icons/workflow.svg").as_slice(),
            Icon::WrapText => include_bytes!("../icons/wrap-text.svg").as_slice(),
            Icon::Wrench => include_bytes!("../icons/wrench.svg").as_slice(),
            Icon::X => include_bytes!("../icons/x.svg").as_slice(),
            Icon::Zap => include_bytes!("../icons/zap.svg").as_slice(),
            Icon::Zed => include_bytes!("../icons/zed.svg").as_slice(),
        }
    }

    /// Lucide kebab-case name (for diagnostics).
    pub fn name(self) -> &'static str {
        match self {
            Icon::Activity => "activity",
            Icon::AlarmClock => "alarm-clock",
            Icon::AppWindow => "app-window",
            Icon::Archive => "archive",
            Icon::ArrowDown => "arrow-down",
            Icon::ArrowLeft => "arrow-left",
            Icon::ArrowUp => "arrow-up",
            Icon::AtSign => "at-sign",
            Icon::Bell => "bell",
            Icon::Bookmark => "bookmark",
            Icon::BookPlus => "book-plus",
            Icon::Bot => "bot",
            Icon::Bug => "bug",
            Icon::CalendarClock => "calendar-clock",
            Icon::ChartNoAxesColumn => "chart-no-axes-column",
            Icon::Check => "check",
            Icon::ChevronDown => "chevron-down",
            Icon::ChevronLeft => "chevron-left",
            Icon::ChevronRight => "chevron-right",
            Icon::ChevronUp => "chevron-up",
            Icon::ChevronsDownUp => "chevrons-down-up",
            Icon::ChevronsUpDown => "chevrons-up-down",
            Icon::Circle => "circle",
            Icon::CircleAlert => "circle-alert",
            Icon::CircleCheck => "circle-check",
            Icon::CircleDashed => "circle-dashed",
            Icon::CircleDot => "circle-dot",
            Icon::CircleDotDashed => "circle-dot-dashed",
            Icon::CircleHelp => "circle-help",
            Icon::CircleStop => "circle-stop",
            Icon::Clock => "clock",
            Icon::CloudUpload => "cloud-upload",
            Icon::CloudDownload => "cloud-download",
            Icon::Copy => "copy",
            Icon::CornerDownLeft => "corner-down-left",
            Icon::Dot => "dot",
            Icon::Download => "download",
            Icon::Ellipsis => "ellipsis",
            Icon::ExternalLink => "external-link",
            Icon::Eye => "eye",
            Icon::File => "file",
            Icon::FileCode => "file-code",
            Icon::FileDiff => "file-diff",
            Icon::FileSearch => "file-search",
            Icon::FileText => "file-text",
            Icon::FlaskConical => "flask-conical",
            Icon::Folder => "folder",
            Icon::FolderGit2 => "folder-git-2",
            Icon::FolderOpen => "folder-open",
            Icon::FolderPlus => "folder-plus",
            Icon::FolderTree => "folder-tree",
            Icon::GitBranch => "git-branch",
            Icon::GitCommitHorizontal => "git-commit-horizontal",
            Icon::GitCompare => "git-compare",
            Icon::GitFork => "git-fork",
            Icon::GitMerge => "git-merge",
            Icon::GitPullRequest => "git-pull-request",
            Icon::Globe => "globe",
            Icon::Hash => "hash",
            Icon::Image => "image",
            Icon::Info => "info",
            Icon::KeyRound => "key-round",
            Icon::Layers => "layers",
            Icon::Keyboard => "keyboard",
            Icon::Link => "link",
            Icon::ListFilter => "list-filter",
            Icon::ListTodo => "list-todo",
            Icon::LoaderCircle => "loader-circle",
            Icon::Lock => "lock",
            Icon::Maximize2 => "maximize-2",
            Icon::MailOpen => "mail-open",
            Icon::MessageSquare => "message-square",
            Icon::MessageCircleQuestion => "message-circle-question",
            Icon::MessageSquareText => "message-square-text",
            Icon::MessagesSquare => "messages-square",
            Icon::MessageSquarePlus => "message-square-plus",
            Icon::MessageSquareDashed => "message-square-dashed",
            Icon::Mic => "mic",
            Icon::Minus => "minus",
            Icon::Monitor => "monitor",
            Icon::Moon => "moon",
            Icon::Palette => "palette",
            Icon::PanelBottom => "panel-bottom",
            Icon::Paperclip => "paperclip",
            Icon::PanelRight => "panel-right",
            Icon::PanelsTopLeft => "panels-top-left",
            Icon::Pause => "pause",
            Icon::PenLine => "pen-line",
            Icon::Pencil => "pencil",
            Icon::Pin => "pin",
            Icon::PinOff => "pin-off",
            Icon::Play => "play",
            Icon::Plug => "plug",
            Icon::Plus => "plus",
            Icon::ProviderClaude => "provider-claude",
            Icon::ProviderOpenAi => "provider-openai",
            Icon::Quote => "quote",
            Icon::RefreshCw => "refresh-cw",
            Icon::ScrollText => "scroll-text",
            Icon::Search => "search",
            Icon::Server => "server",
            Icon::Settings => "settings",
            Icon::Settings2 => "settings-2",
            Icon::Shield => "shield",
            Icon::ShieldCheck => "shield-check",
            Icon::ShieldOff => "shield-off",
            Icon::ShieldQuestion => "shield-question",
            Icon::Smartphone => "smartphone",
            Icon::SnapShot => "snap-shot",
            Icon::Sparkles => "sparkles",
            Icon::Square => "square",
            Icon::SquareArrowOutUpRight => "square-arrow-out-up-right",
            Icon::SquareDashed => "square-dashed",
            Icon::SquareMenu => "square-menu",
            Icon::SquarePen => "square-pen",
            Icon::SquareSplitHorizontal => "square-split-horizontal",
            Icon::SquareSplitVertical => "square-split-vertical",
            Icon::SquareTerminal => "square-terminal",
            Icon::Star => "star",
            Icon::StarFilled => "star-filled",
            Icon::Tag => "tag",
            Icon::Terminal => "terminal",
            Icon::ThumbsDown => "thumbs-down",
            Icon::ThumbsUp => "thumbs-up",
            Icon::Timer => "timer",
            Icon::Trash2 => "trash-2",
            Icon::TriangleAlert => "triangle-alert",
            Icon::Undo2 => "undo-2",
            Icon::Unlink => "unlink",
            Icon::Upload => "upload",
            Icon::Users => "users",
            Icon::UserRound => "user-round",
            Icon::UserPlus => "user-plus",
            Icon::Workflow => "workflow",
            Icon::WrapText => "wrap-text",
            Icon::Wrench => "wrench",
            Icon::X => "x",
            Icon::Zap => "zap",
            Icon::Zed => "zed",
        }
    }
}

/// Render `icon` at `size` px. Color comes from `.text_color(...)` on the
/// returned element — lucide strokes `currentColor`.
#[allow(dead_code)] // Consumed per-surface as icons land.
pub fn icon(icon: Icon, size: f32) -> Svg {
    svg().data(icon.data()).w(px(size)).h(px(size))
}

/// `icon` tinted `base`, recoloring to `hover` while the ancestor hitbox
/// tagged `.group(group)` is hovered — the web's `hover:[&>svg]` /
/// `group-hover:` recolor (`SidebarMenuButton` does
/// `hover:[&>svg]:text-sidebar-foreground`). `Svg::paint` reads its own
/// `style.text.color`, so a parent's `.hover(text_color)` never reaches the
/// glyph; `group_hover` refines the svg's own style at paint time instead.
/// The ancestor must be `Stateful` (`.id(...)`, giving it a hitbox) and
/// `.group(group)`; group push/pop is scoped to the ancestor's paint, so the
/// same name may be reused across sibling buttons.
#[allow(dead_code)] // Consumed per-surface as icons land.
pub fn hover_icon(
    glyph: Icon,
    size: f32,
    base: impl Into<Hsla>,
    hover: impl Into<Hsla>,
    group: &'static str,
) -> Svg {
    let hover = hover.into();
    icon(glyph, size)
        .text_color(base)
        .group_hover(group, move |s| s.text_color(hover))
}
