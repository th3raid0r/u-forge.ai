//! Shared action metadata for every surface that can expose an application action.
//!
//! GPUI still owns dispatch, focus routing, and native menu integration. This
//! module owns the user-facing contract so keybindings, native and in-app
//! menus, tooltips, context entries, and status toggles cannot drift apart.

use gpui::{
    Action, DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate, Menu,
    MenuItem as NativeMenuItem,
};

use crate::ui::icons::IconName;

gpui::actions!([
    SaveActiveItem,
    SaveAllItems,
    OpenLemonadeSetup,
    OpenSettings,
    ToggleSidebar,
    ToggleSearchPanel,
    ToggleRightPanel,
    ToggleDetailsPanel,
    ToggleFocusedPanelZoom,
    FocusNextRegion,
    FocusPreviousRegion,
    WorldNextRow,
    WorldPreviousRow,
    WorldActivateRow,
    WorldDeleteRow,
    WorldOpenContextMenu,
    DetailsNextTab,
    DetailsPreviousTab,
    DetailsCloseTab,
    ClearData,
    ClearSchema,
    ImportData,
    ImportSchema,
    ExportData,
    TogglePerfOverlay,
    FitGraph
]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId {
    SaveActiveItem,
    SaveAllItems,
    OpenLemonadeSetup,
    OpenSettings,
    ToggleSidebar,
    ToggleSearchPanel,
    ToggleRightPanel,
    ToggleDetailsPanel,
    ToggleFocusedPanelZoom,
    FocusNextRegion,
    FocusPreviousRegion,
    WorldNextRow,
    WorldPreviousRow,
    WorldActivateRow,
    WorldDeleteRow,
    WorldOpenContextMenu,
    DetailsNextTab,
    DetailsPreviousTab,
    DetailsCloseTab,
    ClearData,
    ClearSchema,
    ImportData,
    ImportSchema,
    ExportData,
    TogglePerfOverlay,
    FitGraph,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionMenu {
    File,
    View,
}

impl ActionMenu {
    pub const ALL: [Self; 2] = [Self::File, Self::View];

    pub const fn title(self) -> &'static str {
        match self {
            Self::File => "File",
            Self::View => "View",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusSide {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutDescriptor {
    pub key: &'static str,
    pub display: &'static str,
    pub context: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuPlacement {
    pub menu: ActionMenu,
    /// A separator is inserted whenever adjacent entries change section.
    pub section: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusPlacement {
    pub side: StatusSide,
    pub order: u8,
    pub element_id: &'static str,
    pub label: &'static str,
    pub icon: Option<IconName>,
    pub icon_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnableRule {
    Always,
    ActiveDetailsDirty,
    AnyDetailsDirty,
    HasActiveDetailsTab,
    MultipleDetailsTabs,
    HasSchema,
    HasData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectedRule {
    Never,
    WorldOpen,
    SearchOpen,
    AssistantOpen,
    DetailsOpen,
    PerformanceVisible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionTone {
    Normal,
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActionContext {
    pub show_advanced_controls: bool,
    pub has_schema: bool,
    pub has_data: bool,
    pub active_details_dirty: bool,
    pub any_details_dirty: bool,
    pub has_active_details_tab: bool,
    pub details_tab_count: usize,
    pub world_open: bool,
    pub search_open: bool,
    pub assistant_open: bool,
    pub details_open: bool,
    pub performance_visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionDescriptor {
    pub id: ActionId,
    pub element_id: &'static str,
    pub label: &'static str,
    pub tooltip: &'static str,
    pub shortcut: Option<ShortcutDescriptor>,
    pub menu: Option<MenuPlacement>,
    pub status: Option<StatusPlacement>,
    pub advanced: bool,
    pub tone: ActionTone,
    enable_rule: EnableRule,
    selected_rule: SelectedRule,
}

impl ActionDescriptor {
    pub fn action(self) -> Box<dyn Action> {
        match self.id {
            ActionId::SaveActiveItem => Box::new(SaveActiveItem),
            ActionId::SaveAllItems => Box::new(SaveAllItems),
            ActionId::OpenLemonadeSetup => Box::new(OpenLemonadeSetup),
            ActionId::OpenSettings => Box::new(OpenSettings),
            ActionId::ToggleSidebar => Box::new(ToggleSidebar),
            ActionId::ToggleSearchPanel => Box::new(ToggleSearchPanel),
            ActionId::ToggleRightPanel => Box::new(ToggleRightPanel),
            ActionId::ToggleDetailsPanel => Box::new(ToggleDetailsPanel),
            ActionId::ToggleFocusedPanelZoom => Box::new(ToggleFocusedPanelZoom),
            ActionId::FocusNextRegion => Box::new(FocusNextRegion),
            ActionId::FocusPreviousRegion => Box::new(FocusPreviousRegion),
            ActionId::WorldNextRow => Box::new(WorldNextRow),
            ActionId::WorldPreviousRow => Box::new(WorldPreviousRow),
            ActionId::WorldActivateRow => Box::new(WorldActivateRow),
            ActionId::WorldDeleteRow => Box::new(WorldDeleteRow),
            ActionId::WorldOpenContextMenu => Box::new(WorldOpenContextMenu),
            ActionId::DetailsNextTab => Box::new(DetailsNextTab),
            ActionId::DetailsPreviousTab => Box::new(DetailsPreviousTab),
            ActionId::DetailsCloseTab => Box::new(DetailsCloseTab),
            ActionId::ClearData => Box::new(ClearData),
            ActionId::ClearSchema => Box::new(ClearSchema),
            ActionId::ImportData => Box::new(ImportData),
            ActionId::ImportSchema => Box::new(ImportSchema),
            ActionId::ExportData => Box::new(ExportData),
            ActionId::TogglePerfOverlay => Box::new(TogglePerfOverlay),
            ActionId::FitGraph => Box::new(FitGraph),
        }
    }

    pub fn is_visible(self, context: &ActionContext) -> bool {
        !self.advanced || context.show_advanced_controls
    }

    pub fn is_enabled(self, context: &ActionContext) -> bool {
        self.is_visible(context)
            && match self.enable_rule {
                EnableRule::Always => true,
                EnableRule::ActiveDetailsDirty => context.active_details_dirty,
                EnableRule::AnyDetailsDirty => context.any_details_dirty,
                EnableRule::HasActiveDetailsTab => context.has_active_details_tab,
                EnableRule::MultipleDetailsTabs => context.details_tab_count > 1,
                EnableRule::HasSchema => context.has_schema,
                EnableRule::HasData => context.has_data,
            }
    }

    pub fn is_selected(self, context: &ActionContext) -> bool {
        match self.selected_rule {
            SelectedRule::Never => false,
            SelectedRule::WorldOpen => context.world_open,
            SelectedRule::SearchOpen => context.search_open,
            SelectedRule::AssistantOpen => context.assistant_open,
            SelectedRule::DetailsOpen => context.details_open,
            SelectedRule::PerformanceVisible => context.performance_visible,
        }
    }

    pub fn display_tooltip(self) -> String {
        match self.shortcut {
            Some(shortcut) => format!("{} ({})", self.tooltip, shortcut.display),
            None => self.tooltip.to_string(),
        }
    }
}

const fn shortcut(
    key: &'static str,
    display: &'static str,
    context: Option<&'static str>,
) -> Option<ShortcutDescriptor> {
    Some(ShortcutDescriptor {
        key,
        display,
        context,
    })
}

const fn menu(menu: ActionMenu, section: u8) -> Option<MenuPlacement> {
    Some(MenuPlacement { menu, section })
}

const fn status(
    side: StatusSide,
    order: u8,
    element_id: &'static str,
    label: &'static str,
    icon: Option<IconName>,
    icon_only: bool,
) -> Option<StatusPlacement> {
    Some(StatusPlacement {
        side,
        order,
        element_id,
        label,
        icon,
        icon_only,
    })
}

macro_rules! descriptor {
    ($id:ident, $element_id:literal, $label:literal, $tooltip:literal,
     $shortcut:expr, $menu:expr, $status:expr, $advanced:expr, $tone:ident,
     $enabled:ident, $selected:ident) => {
        ActionDescriptor {
            id: ActionId::$id,
            element_id: $element_id,
            label: $label,
            tooltip: $tooltip,
            shortcut: $shortcut,
            menu: $menu,
            status: $status,
            advanced: $advanced,
            tone: ActionTone::$tone,
            enable_rule: EnableRule::$enabled,
            selected_rule: SelectedRule::$selected,
        }
    };
}

pub const ACTION_DESCRIPTORS: &[ActionDescriptor] = &[
    descriptor!(
        SaveActiveItem,
        "save-item",
        "Save Changes",
        "Save the active Details item",
        shortcut("ctrl-s", "Ctrl+S", None),
        menu(ActionMenu::File, 0),
        None,
        false,
        Normal,
        ActiveDetailsDirty,
        Never
    ),
    descriptor!(
        SaveAllItems,
        "save-all-item",
        "Save All",
        "Save every changed Details item",
        shortcut("ctrl-shift-s", "Ctrl+Shift+S", None),
        menu(ActionMenu::File, 0),
        None,
        false,
        Normal,
        AnyDetailsDirty,
        Never
    ),
    descriptor!(
        OpenLemonadeSetup,
        "lemonade-setup-item",
        "Lemonade AI Setup…",
        "Set up local AI capabilities",
        None,
        menu(ActionMenu::File, 1),
        None,
        false,
        Normal,
        Always,
        Never
    ),
    descriptor!(
        ImportSchema,
        "import-schema-item",
        "Import Schema…",
        "Import an authoritative world schema",
        None,
        menu(ActionMenu::File, 2),
        None,
        false,
        Normal,
        Always,
        Never
    ),
    descriptor!(
        ImportData,
        "import-data-item",
        "Import Data…",
        "Import world data using the loaded schema",
        None,
        menu(ActionMenu::File, 2),
        None,
        false,
        Normal,
        HasSchema,
        Never
    ),
    descriptor!(
        ExportData,
        "export-data-item",
        "Export Data…",
        "Export the current world data",
        None,
        menu(ActionMenu::File, 2),
        None,
        false,
        Normal,
        HasData,
        Never
    ),
    descriptor!(
        ClearSchema,
        "clear-schema-item",
        "Clear Schema",
        "Remove the loaded world schema",
        None,
        menu(ActionMenu::File, 3),
        None,
        false,
        Danger,
        HasSchema,
        Never
    ),
    descriptor!(
        ClearData,
        "clear-data-item",
        "Clear Data",
        "Remove all world items and relationships",
        None,
        menu(ActionMenu::File, 3),
        None,
        false,
        Danger,
        HasData,
        Never
    ),
    descriptor!(
        ToggleSidebar,
        "toggle-world-item",
        "World",
        "Show or hide the World panel",
        shortcut("ctrl-b", "Ctrl+B", None),
        menu(ActionMenu::View, 0),
        status(
            StatusSide::Left,
            0,
            "status-world",
            "World",
            Some(IconName::World),
            true
        ),
        false,
        Normal,
        Always,
        WorldOpen
    ),
    descriptor!(
        ToggleSearchPanel,
        "toggle-search-item",
        "Search",
        "Show or hide the Search panel",
        shortcut("ctrl-shift-f", "Ctrl+Shift+F", None),
        menu(ActionMenu::View, 0),
        status(
            StatusSide::Left,
            1,
            "status-search",
            "Search",
            Some(IconName::Search),
            true
        ),
        false,
        Normal,
        Always,
        SearchOpen
    ),
    descriptor!(
        ToggleRightPanel,
        "toggle-assistant-item",
        "Assistant",
        "Show or hide the Assistant panel",
        shortcut("ctrl-j", "Ctrl+J", None),
        menu(ActionMenu::View, 0),
        status(
            StatusSide::Right,
            1,
            "status-assistant",
            "Assistant",
            Some(IconName::Bot),
            true
        ),
        false,
        Normal,
        Always,
        AssistantOpen
    ),
    descriptor!(
        ToggleDetailsPanel,
        "toggle-details-item",
        "Details",
        "Show or hide the Details panel",
        shortcut("ctrl-shift-j", "Ctrl+Shift+J", None),
        menu(ActionMenu::View, 0),
        status(
            StatusSide::Right,
            0,
            "status-details",
            "Details",
            Some(IconName::Edit),
            true
        ),
        false,
        Normal,
        Always,
        DetailsOpen
    ),
    descriptor!(
        ToggleFocusedPanelZoom,
        "toggle-panel-zoom-item",
        "Maximize Focused Panel",
        "Maximize or restore the focused dock panel",
        shortcut("ctrl-shift-m", "Ctrl+Shift+M", None),
        menu(ActionMenu::View, 1),
        None,
        false,
        Normal,
        Always,
        Never
    ),
    descriptor!(
        OpenSettings,
        "open-settings-item",
        "Settings…",
        "Open interface settings",
        shortcut("ctrl-,", "Ctrl+,", None),
        menu(ActionMenu::View, 1),
        None,
        false,
        Normal,
        Always,
        Never
    ),
    descriptor!(
        FitGraph,
        "fit-connections-item",
        "Fit Connections",
        "Fit all Connections items in the World Canvas",
        shortcut("ctrl-shift-0", "Ctrl+Shift+0", None),
        menu(ActionMenu::View, 1),
        None,
        false,
        Normal,
        HasData,
        Never
    ),
    descriptor!(
        TogglePerfOverlay,
        "toggle-perf-item",
        "Performance Diagnostics",
        "Toggle performance diagnostics",
        shortcut("ctrl-shift-p", "Ctrl+Shift+P", None),
        menu(ActionMenu::View, 2),
        status(StatusSide::Right, 2, "status-perf", "Perf", None, false),
        true,
        Normal,
        Always,
        PerformanceVisible
    ),
    descriptor!(
        FocusNextRegion,
        "focus-next-region",
        "Focus Next Region",
        "Move focus to the next workspace region",
        shortcut("f6", "F6", None),
        None,
        None,
        false,
        Normal,
        Always,
        Never
    ),
    descriptor!(
        FocusPreviousRegion,
        "focus-previous-region",
        "Focus Previous Region",
        "Move focus to the previous workspace region",
        shortcut("shift-f6", "Shift+F6", None),
        None,
        None,
        false,
        Normal,
        Always,
        Never
    ),
    descriptor!(
        WorldNextRow,
        "world-next-row",
        "Next World Row",
        "Move to the next World row",
        shortcut("down", "Down", Some("WorldPanel")),
        None,
        None,
        false,
        Normal,
        Always,
        Never
    ),
    descriptor!(
        WorldPreviousRow,
        "world-previous-row",
        "Previous World Row",
        "Move to the previous World row",
        shortcut("up", "Up", Some("WorldPanel")),
        None,
        None,
        false,
        Normal,
        Always,
        Never
    ),
    descriptor!(
        WorldActivateRow,
        "world-activate-row",
        "Open World Row",
        "Open or expand the current World row",
        shortcut("enter", "Enter", Some("WorldPanel")),
        None,
        None,
        false,
        Normal,
        Always,
        Never
    ),
    descriptor!(
        WorldDeleteRow,
        "world-delete-row",
        "Delete World Item",
        "Delete the current World item",
        shortcut("delete", "Delete", Some("WorldPanel")),
        None,
        None,
        false,
        Danger,
        Always,
        Never
    ),
    descriptor!(
        WorldOpenContextMenu,
        "world-open-context-menu",
        "World Context Menu",
        "Open actions for the current World row",
        shortcut("shift-f10", "Shift+F10", Some("WorldPanel")),
        None,
        None,
        false,
        Normal,
        Always,
        Never
    ),
    descriptor!(
        DetailsNextTab,
        "details-next-tab",
        "Next Details Tab",
        "Activate the next Details tab",
        shortcut("ctrl-pagedown", "Ctrl+PageDown", Some("DetailsPanel")),
        None,
        None,
        false,
        Normal,
        MultipleDetailsTabs,
        Never
    ),
    descriptor!(
        DetailsPreviousTab,
        "details-previous-tab",
        "Previous Details Tab",
        "Activate the previous Details tab",
        shortcut("ctrl-pageup", "Ctrl+PageUp", Some("DetailsPanel")),
        None,
        None,
        false,
        Normal,
        MultipleDetailsTabs,
        Never
    ),
    descriptor!(
        DetailsCloseTab,
        "details-close-tab",
        "Close Details Tab",
        "Close the active Details tab",
        shortcut("ctrl-w", "Ctrl+W", Some("DetailsPanel")),
        None,
        None,
        false,
        Normal,
        HasActiveDetailsTab,
        Never
    ),
];

pub fn descriptor(id: ActionId) -> &'static ActionDescriptor {
    ACTION_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.id == id)
        .expect("every ActionId must have a descriptor")
}

pub fn action_key_bindings() -> Vec<KeyBinding> {
    ACTION_DESCRIPTORS
        .iter()
        .filter_map(|descriptor| {
            let shortcut = descriptor.shortcut?;
            let context_predicate = shortcut.context.map(|context| {
                KeyBindingContextPredicate::parse(context)
                    .expect("action descriptor key context must be valid")
                    .into()
            });
            Some(
                KeyBinding::load(
                    shortcut.key,
                    descriptor.action(),
                    context_predicate,
                    false,
                    None,
                    &DummyKeyboardMapper,
                )
                .expect("action descriptor shortcut must use valid GPUI syntax"),
            )
        })
        .collect()
}

pub fn native_menus(context: &ActionContext) -> Vec<Menu> {
    ActionMenu::ALL
        .into_iter()
        .map(|menu_kind| {
            let descriptors: Vec<_> = ACTION_DESCRIPTORS
                .iter()
                .copied()
                .filter(|descriptor| {
                    descriptor.menu.is_some_and(|menu| menu.menu == menu_kind)
                        && descriptor.is_visible(context)
                })
                .collect();
            let mut previous_section = None;
            let mut items = Vec::new();
            for descriptor in descriptors {
                let placement = descriptor.menu.expect("menu descriptor was filtered above");
                if previous_section.is_some_and(|section| section != placement.section) {
                    items.push(NativeMenuItem::separator());
                }
                items.push(NativeMenuItem::Action {
                    name: descriptor.label.into(),
                    action: descriptor.action(),
                    os_action: None,
                    checked: descriptor.is_selected(context),
                });
                previous_section = Some(placement.section);
            }
            Menu {
                name: menu_kind.title().into(),
                items,
            }
        })
        .collect()
}

pub fn menu_descriptors(
    menu: ActionMenu,
    context: &ActionContext,
) -> impl Iterator<Item = &'static ActionDescriptor> {
    ACTION_DESCRIPTORS.iter().filter(move |descriptor| {
        descriptor
            .menu
            .is_some_and(|placement| placement.menu == menu)
            && descriptor.is_visible(context)
    })
}

pub fn status_descriptors(
    side: StatusSide,
    context: &ActionContext,
) -> std::vec::IntoIter<&'static ActionDescriptor> {
    let mut descriptors: Vec<_> = ACTION_DESCRIPTORS
        .iter()
        .filter(|descriptor| {
            descriptor.status.is_some_and(|status| status.side == side)
                && descriptor.is_visible(context)
        })
        .collect();
    descriptors.sort_by_key(|descriptor| {
        descriptor
            .status
            .expect("status descriptor was filtered above")
            .order
    });
    descriptors.into_iter()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextScope {
    WorldGroup,
    WorldItem,
    DetailsTab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextActionId {
    ToggleWorldGroup,
    NewWorldItem,
    OpenDetails,
    DeleteWorldItem,
    ToggleTabPinned,
    MoveTabLeft,
    MoveTabRight,
    CloseTab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextEnableRule {
    Always,
    TabClean,
    CanMoveTabLeft,
    CanMoveTabRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextActionState {
    pub tab_dirty: bool,
    pub can_move_tab_left: bool,
    pub can_move_tab_right: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextActionDescriptor {
    pub id: ContextActionId,
    pub scope: ContextScope,
    pub element_id: &'static str,
    pub label: &'static str,
    pub alternate_label: Option<&'static str>,
    pub tooltip: &'static str,
    pub section: u8,
    pub tone: ActionTone,
    enable_rule: ContextEnableRule,
}

impl ContextActionDescriptor {
    pub fn is_enabled(self, state: &ContextActionState) -> bool {
        match self.enable_rule {
            ContextEnableRule::Always => true,
            ContextEnableRule::TabClean => !state.tab_dirty,
            ContextEnableRule::CanMoveTabLeft => state.can_move_tab_left,
            ContextEnableRule::CanMoveTabRight => state.can_move_tab_right,
        }
    }
}

pub const CONTEXT_ACTION_DESCRIPTORS: &[ContextActionDescriptor] = &[
    ContextActionDescriptor {
        id: ContextActionId::ToggleWorldGroup,
        scope: ContextScope::WorldGroup,
        element_id: "world-toggle-group",
        label: "Show / Hide Group",
        alternate_label: None,
        tooltip: "Expand or collapse this World group",
        section: 0,
        tone: ActionTone::Normal,
        enable_rule: ContextEnableRule::Always,
    },
    ContextActionDescriptor {
        id: ContextActionId::NewWorldItem,
        scope: ContextScope::WorldGroup,
        element_id: "world-new-item",
        label: "New World Item",
        alternate_label: None,
        tooltip: "Create a new item in this World group",
        section: 0,
        tone: ActionTone::Normal,
        enable_rule: ContextEnableRule::Always,
    },
    ContextActionDescriptor {
        id: ContextActionId::OpenDetails,
        scope: ContextScope::WorldItem,
        element_id: "world-open-details",
        label: "Open Details",
        alternate_label: None,
        tooltip: "Open this item in Details",
        section: 0,
        tone: ActionTone::Normal,
        enable_rule: ContextEnableRule::Always,
    },
    ContextActionDescriptor {
        id: ContextActionId::DeleteWorldItem,
        scope: ContextScope::WorldItem,
        element_id: "world-delete-item",
        label: "Delete…",
        alternate_label: None,
        tooltip: "Delete this World item",
        section: 0,
        tone: ActionTone::Danger,
        enable_rule: ContextEnableRule::Always,
    },
    ContextActionDescriptor {
        id: ContextActionId::ToggleTabPinned,
        scope: ContextScope::DetailsTab,
        element_id: "details-toggle-pinned",
        label: "Keep Open",
        alternate_label: Some("Allow Preview Replacement"),
        tooltip: "Choose whether selections can replace this preview tab",
        section: 0,
        tone: ActionTone::Normal,
        enable_rule: ContextEnableRule::TabClean,
    },
    ContextActionDescriptor {
        id: ContextActionId::MoveTabLeft,
        scope: ContextScope::DetailsTab,
        element_id: "details-move-left",
        label: "Move Left",
        alternate_label: None,
        tooltip: "Move this Details tab left",
        section: 0,
        tone: ActionTone::Normal,
        enable_rule: ContextEnableRule::CanMoveTabLeft,
    },
    ContextActionDescriptor {
        id: ContextActionId::MoveTabRight,
        scope: ContextScope::DetailsTab,
        element_id: "details-move-right",
        label: "Move Right",
        alternate_label: None,
        tooltip: "Move this Details tab right",
        section: 0,
        tone: ActionTone::Normal,
        enable_rule: ContextEnableRule::CanMoveTabRight,
    },
    ContextActionDescriptor {
        id: ContextActionId::CloseTab,
        scope: ContextScope::DetailsTab,
        element_id: "details-close",
        label: "Close",
        alternate_label: None,
        tooltip: "Close this Details tab",
        section: 1,
        tone: ActionTone::Normal,
        enable_rule: ContextEnableRule::Always,
    },
];

pub fn context_descriptors(
    scope: ContextScope,
) -> impl Iterator<Item = &'static ContextActionDescriptor> {
    CONTEXT_ACTION_DESCRIPTORS
        .iter()
        .filter(move |descriptor| descriptor.scope == scope)
}

#[cfg(test)]
mod tests {
    use gpui::Keystroke;

    use super::*;

    #[test]
    fn every_declared_shortcut_uses_valid_gpui_syntax() {
        for descriptor in ACTION_DESCRIPTORS {
            if let Some(shortcut) = descriptor.shortcut {
                assert!(
                    Keystroke::parse(shortcut.key).is_ok(),
                    "invalid shortcut for {:?}: {}",
                    descriptor.id,
                    shortcut.key
                );
            }
        }
    }

    #[test]
    fn save_and_data_actions_share_enablement_rules() {
        let mut context = ActionContext::default();
        assert!(!descriptor(ActionId::SaveActiveItem).is_enabled(&context));
        assert!(!descriptor(ActionId::SaveAllItems).is_enabled(&context));
        assert!(!descriptor(ActionId::ImportData).is_enabled(&context));
        assert!(!descriptor(ActionId::ExportData).is_enabled(&context));

        context.active_details_dirty = true;
        context.any_details_dirty = true;
        context.has_schema = true;
        context.has_data = true;
        assert!(descriptor(ActionId::SaveActiveItem).is_enabled(&context));
        assert!(descriptor(ActionId::SaveAllItems).is_enabled(&context));
        assert!(descriptor(ActionId::ImportData).is_enabled(&context));
        assert!(descriptor(ActionId::ExportData).is_enabled(&context));
    }

    #[test]
    fn advanced_actions_are_hidden_from_every_generated_surface() {
        let context = ActionContext::default();
        assert!(!descriptor(ActionId::TogglePerfOverlay).is_visible(&context));
        assert!(
            menu_descriptors(ActionMenu::View, &context)
                .all(|descriptor| descriptor.id != ActionId::TogglePerfOverlay)
        );
        assert!(
            status_descriptors(StatusSide::Right, &context)
                .all(|descriptor| descriptor.id != ActionId::TogglePerfOverlay)
        );

        let context = ActionContext {
            show_advanced_controls: true,
            ..ActionContext::default()
        };
        assert!(descriptor(ActionId::TogglePerfOverlay).is_visible(&context));
        assert!(
            menu_descriptors(ActionMenu::View, &context)
                .any(|descriptor| descriptor.id == ActionId::TogglePerfOverlay)
        );
        assert!(
            status_descriptors(StatusSide::Right, &context)
                .any(|descriptor| descriptor.id == ActionId::TogglePerfOverlay)
        );
    }

    #[test]
    fn context_entry_enablement_comes_from_descriptors() {
        let state = ContextActionState {
            tab_dirty: true,
            can_move_tab_left: false,
            can_move_tab_right: true,
        };
        let entries: Vec<_> = context_descriptors(ContextScope::DetailsTab).collect();
        assert!(!entries[0].is_enabled(&state));
        assert!(!entries[1].is_enabled(&state));
        assert!(entries[2].is_enabled(&state));
        assert!(entries[3].is_enabled(&state));
    }
}
