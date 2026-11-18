//! The settings dialog (gear button in the header bar). Every change is
//! applied to all windows at once and written back to `config.toml`,
//! keeping the user's comments.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use tudi_core::config::{Language, SortBy};
use tudi_core::frecency::ExcludeRules;

use crate::browser::Browser;
use crate::history::Snapshot;
use crate::i18n::tr;

pub struct Settings {
    dialog: adw::PreferencesDialog,
    rules_group: adw::PreferencesGroup,
    rule_rows: RefCell<Vec<adw::ActionRow>>,
    excl_group: adw::PreferencesGroup,
    excl_rows: RefCell<Vec<gtk::Widget>>,
    browser: Weak<Browser>,
}

fn switch_row(title: &str, subtitle: &str, active: bool) -> adw::SwitchRow {
    let r = adw::SwitchRow::builder()
        .title(title)
        .active(active)
        .build();
    if !subtitle.is_empty() {
        r.set_subtitle(subtitle);
    }
    r
}

fn combo_row(title: &str, items: &[String], selected: u32) -> adw::ComboRow {
    let strs: Vec<&str> = items.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&strs);
    adw::ComboRow::builder()
        .title(title)
        .model(&model)
        .selected(selected)
        .build()
}

impl Settings {
    pub fn open(b: &Rc<Browser>) -> Rc<Self> {
        let cfg = b.shared.config.borrow().clone();
        let dialog = adw::PreferencesDialog::new();
        dialog.set_title(&tr("Settings"));
        dialog.set_search_enabled(false);

        // ---- General
        let general = adw::PreferencesPage::builder()
            .name("general")
            .title(tr("General"))
            .icon_name("preferences-system-symbolic")
            .build();
        let ui = adw::PreferencesGroup::builder()
            .title(tr("Interface"))
            .build();
        let lang = combo_row(
            &tr("Language"),
            &[tr("English"), tr("System language")],
            u32::from(cfg.ui.language == Language::System),
        );
        lang.set_subtitle(&tr("Takes effect after restarting tudi"));
        ui.add(&lang);
        let zoom = adw::SpinRow::with_range(
            f64::from(crate::zoom::MIN),
            f64::from(crate::zoom::MAX),
            f64::from(crate::zoom::STEP),
        );
        zoom.set_title(&tr("Zoom (%)"));
        zoom.set_subtitle("Ctrl+= / Ctrl+- / Ctrl+0");
        zoom.set_value(f64::from(crate::zoom::level()));
        ui.add(&zoom);
        general.add(&ui);

        let browse = adw::PreferencesGroup::builder()
            .title(tr("Browsing"))
            .build();
        let hidden = switch_row(&tr("Show hidden files"), "Ctrl+H", cfg.browse.show_hidden);
        let dirs_first = switch_row(&tr("Folders before files"), "", cfg.browse.dirs_first);
        let sort = combo_row(
            &tr("Sort by"),
            &[tr("Name"), tr("Size"), tr("Modified")],
            match cfg.browse.sort {
                SortBy::Name => 0,
                SortBy::Size => 1,
                SortBy::Mtime => 2,
            },
        );
        browse.add(&hidden);
        browse.add(&dirs_first);
        browse.add(&sort);
        general.add(&browse);
        dialog.add(&general);

        // ---- Commands
        let commands = adw::PreferencesPage::builder()
            .name("commands")
            .title(tr("Commands"))
            .icon_name("utilities-terminal-symbolic")
            .build();
        let cmd = adw::PreferencesGroup::builder()
            .title(tr("External programs"))
            .description(tr(
                "Arguments separated by spaces. {dir} is the current folder; {path} the selected items.",
            ))
            .build();
        let terminal = adw::EntryRow::builder()
            .title(tr("Terminal (Ctrl+Alt+T)"))
            .text(cfg.commands.terminal.join(" "))
            .show_apply_button(true)
            .build();
        let editor = adw::EntryRow::builder()
            .title(tr("Editor (Ctrl+E)"))
            .text(cfg.commands.editor.join(" "))
            .show_apply_button(true)
            .build();
        cmd.add(&terminal);
        cmd.add(&editor);
        commands.add(&cmd);
        dialog.add(&commands);

        // ---- History
        let history = adw::PreferencesPage::builder()
            .name("history")
            .title(tr("History"))
            .icon_name("document-open-recent-symbolic")
            .build();
        let rules_group = adw::PreferencesGroup::builder()
            .title(tr("Never record folders matching"))
            .description(tr(
                "Glob patterns on absolute paths, e.g. /tmp/** or **/node_modules/**",
            ))
            .build();
        let add_rule = adw::EntryRow::builder()
            .title(tr("Add pattern"))
            .show_apply_button(true)
            .build();
        rules_group.add(&add_rule);
        history.add(&rules_group);
        let excl_group = adw::PreferencesGroup::builder()
            .title(tr("Excluded folders"))
            .description(tr(
                "Folders hidden from the bar with “Never show in history”",
            ))
            .build();
        history.add(&excl_group);
        let danger = adw::PreferencesGroup::new();
        let clear_row = adw::ActionRow::builder()
            .title(tr("Clear history"))
            .subtitle(tr("Forget all folder visits; exclusions are kept"))
            .build();
        let clear_btn = gtk::Button::with_label(&tr("Clear…"));
        clear_btn.add_css_class("destructive-action");
        clear_btn.set_valign(gtk::Align::Center);
        clear_row.add_suffix(&clear_btn);
        danger.add(&clear_row);
        history.add(&danger);
        dialog.add(&history);

        let s = Rc::new(Settings {
            dialog: dialog.clone(),
            rules_group,
            rule_rows: RefCell::default(),
            excl_group,
            excl_rows: RefCell::default(),
            browser: Rc::downgrade(b),
        });
        s.refresh_rules();
        s.refresh_exclusions(&b.hist.borrow());

        // ---- handlers
        let w = Rc::downgrade(&s);
        lang.connect_selected_notify(move |r| {
            if let Some(s) = w.upgrade() {
                s.edit(|c| {
                    c.ui.language = if r.selected() == 1 {
                        Language::System
                    } else {
                        Language::En
                    }
                });
                s.toast(&tr("Restart tudi to change the language (tudi --quit)"));
            }
        });
        let w = Rc::downgrade(&s);
        zoom.connect_value_notify(move |r| {
            if let Some(s) = w.upgrade()
                && let Some(b) = s.browser.upgrade()
            {
                b.show_zoom(crate::zoom::set(r.value() as u32));
            }
        });
        let w = Rc::downgrade(&s);
        hidden.connect_active_notify(move |r| {
            if let Some(s) = w.upgrade() {
                s.edit(|c| c.browse.show_hidden = r.is_active());
                s.apply_browse();
            }
        });
        let w = Rc::downgrade(&s);
        dirs_first.connect_active_notify(move |r| {
            if let Some(s) = w.upgrade() {
                s.edit(|c| c.browse.dirs_first = r.is_active());
                s.apply_browse();
            }
        });
        let w = Rc::downgrade(&s);
        sort.connect_selected_notify(move |r| {
            if let Some(s) = w.upgrade() {
                s.edit(|c| {
                    c.browse.sort = match r.selected() {
                        1 => SortBy::Size,
                        2 => SortBy::Mtime,
                        _ => SortBy::Name,
                    }
                });
                s.apply_browse();
            }
        });
        let w = Rc::downgrade(&s);
        terminal.connect_apply(move |e| {
            if let Some(s) = w.upgrade() {
                let v = e.text().split_whitespace().map(String::from).collect();
                s.edit(|c| c.commands.terminal = v);
            }
        });
        let w = Rc::downgrade(&s);
        editor.connect_apply(move |e| {
            if let Some(s) = w.upgrade() {
                let v = e.text().split_whitespace().map(String::from).collect();
                s.edit(|c| c.commands.editor = v);
            }
        });
        let w = Rc::downgrade(&s);
        add_rule.connect_apply(move |e| {
            let Some(s) = w.upgrade() else { return };
            let pat = e.text().trim().to_string();
            if pat.is_empty() {
                return;
            }
            if let Err(err) = ExcludeRules::new(std::slice::from_ref(&pat)) {
                s.toast(&err.to_string());
                return;
            }
            e.set_text("");
            s.edit(|c| {
                if !c.history.exclude.contains(&pat) {
                    c.history.exclude.push(pat);
                }
            });
            s.rules_changed();
        });
        let w = Rc::downgrade(&s);
        clear_btn.connect_clicked(move |_| {
            if let Some(s) = w.upgrade() {
                s.confirm_clear();
            }
        });

        let w = Rc::downgrade(b);
        dialog.connect_closed(move |_| {
            if let Some(b) = w.upgrade() {
                b.settings.replace(None);
                b.drop_settings();
            }
        });
        dialog.present(Some(&b.window));
        b.shared.history.query(b.worker.reply_sender());
        s
    }

    /// Show a page by name (test hook).
    pub fn show_page(&self, name: &str) {
        self.dialog.set_visible_page_name(name);
    }

    fn toast(&self, text: &str) {
        self.dialog.add_toast(adw::Toast::new(text));
    }

    /// Change the shared config and save it.
    fn edit(&self, f: impl FnOnce(&mut tudi_core::config::Config)) {
        if let Some(b) = self.browser.upgrade() {
            f(&mut b.shared.config.borrow_mut());
            b.shared.save_config();
        }
    }

    fn apply_browse(&self) {
        if let Some(b) = self.browser.upgrade() {
            let browse = b.shared.config.borrow().browse.clone();
            for w in b.shared.each() {
                w.apply_browse(&browse);
            }
        }
    }

    fn rules_changed(&self) {
        if let Some(b) = self.browser.upgrade() {
            let rules = b.shared.config.borrow().history.exclude.clone();
            b.shared.history.set_rules(rules);
            b.shared.history_changed();
        }
        self.refresh_rules();
    }

    fn refresh_rules(&self) {
        for r in self.rule_rows.borrow_mut().drain(..) {
            self.rules_group.remove(&r);
        }
        let Some(b) = self.browser.upgrade() else {
            return;
        };
        let rules = b.shared.config.borrow().history.exclude.clone();
        let mut rows = self.rule_rows.borrow_mut();
        for pat in rules {
            let row = adw::ActionRow::builder().title(&pat).build();
            let del = gtk::Button::from_icon_name("user-trash-symbolic");
            del.add_css_class("flat");
            del.set_valign(gtk::Align::Center);
            del.set_tooltip_text(Some(&tr("Remove pattern")));
            let w = self.browser.clone();
            del.connect_clicked(move |_| {
                if let Some(b) = w.upgrade() {
                    b.shared
                        .config
                        .borrow_mut()
                        .history
                        .exclude
                        .retain(|p| *p != pat);
                    b.shared.save_config();
                    let rules = b.shared.config.borrow().history.exclude.clone();
                    b.shared.history.set_rules(rules);
                    b.shared.history_changed();
                    let open = b.settings.borrow().as_ref().and_then(|s| s.upgrade());
                    if let Some(s) = open {
                        s.refresh_rules();
                    }
                }
            });
            row.add_suffix(&del);
            self.rules_group.add(&row);
            rows.push(row);
        }
    }

    /// Show the stored exclusions (called when a history snapshot arrives).
    pub fn refresh_exclusions(&self, snap: &Snapshot) {
        for r in self.excl_rows.borrow_mut().drain(..) {
            self.excl_group.remove(&r);
        }
        let mut rows = self.excl_rows.borrow_mut();
        if snap.exclusions.is_empty() {
            let row = adw::ActionRow::builder().title(tr("None")).build();
            row.add_css_class("dim-label");
            self.excl_group.add(&row);
            rows.push(row.upcast());
            return;
        }
        for x in &snap.exclusions {
            let row = adw::ActionRow::builder()
                .title(x.path.to_string_lossy())
                .subtitle(if x.recursive {
                    tr("with subfolders")
                } else {
                    tr("this folder only")
                })
                .build();
            let allow = gtk::Button::with_label(&tr("Allow"));
            allow.set_valign(gtk::Align::Center);
            allow.set_tooltip_text(Some(&tr("Allow in history again")));
            let (w, p) = (self.browser.clone(), x.path.clone());
            allow.connect_clicked(move |_| {
                if let Some(b) = w.upgrade() {
                    b.shared.history.unexclude(p.clone());
                    b.shared.history_changed();
                }
            });
            row.add_suffix(&allow);
            self.excl_group.add(&row);
            rows.push(row.upcast());
        }
    }

    fn confirm_clear(&self) {
        let ask = adw::AlertDialog::builder()
            .heading(tr("Clear history?"))
            .body(tr(
                "All folder visit counts are forgotten. Exclusions and bookmarks are kept.",
            ))
            .default_response("cancel")
            .close_response("cancel")
            .build();
        ask.add_responses(&[("cancel", &tr("Cancel")), ("clear", &tr("Clear"))]);
        ask.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        let w = self.browser.clone();
        ask.connect_response(None, move |_, r| {
            if r == "clear"
                && let Some(b) = w.upgrade()
            {
                b.shared.history.clear();
                b.shared.history_changed();
            }
        });
        ask.present(Some(&self.dialog));
    }
}
