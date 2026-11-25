use gpui::prelude::*;
use gpui::{App, Context, Entity, Pixels, Render, Window, deferred, div};
use input::{CloseWindow, MinimizeWindow};
use state::{Playback, Sonora};
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use ui::WindowFrame;
use ui::{ActiveTheme as _, Themed};

use crate::chrome::{TitleBar, TitleBarOptions, ToastStack};
use crate::root::{Root, middle_scrolled, ui_font};
use crate::shared::ambient::{self, Ambient};
use crate::shells::Shell as _;
use crate::shells::fullscreen::FullscreenView;

/// The root of the picture-in-picture window: the fullscreen player under a title bar, over an
/// ambient field of its own. It wears the fullscreen theme for the playing cover whatever the
/// main window wears.
pub struct PipView {
    playback: Entity<Playback>,
    title_bar: Entity<TitleBar>,
    field: Entity<Ambient>,
    player: Entity<FullscreenView>,
    /// The toasts, drawn here only while there is no main window to show them.
    toasts: Entity<ToastStack>,
    background: gpui::WindowBackgroundAppearance,
}

impl PipView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sonora = Sonora::global(cx);
        let (playback, queue, settings) = (
            sonora.playback.clone(),
            sonora.queue.clone(),
            sonora.settings.clone(),
        );
        cx.observe(&playback, |_, _, cx| cx.notify()).detach();
        cx.observe(&settings, |_, _, cx| cx.notify()).detach();
        let field = cx.new(Ambient::following);
        cx.observe(&field, |_, _, cx| cx.notify()).detach();

        let player = cx.new(|cx| FullscreenView::new(playback.clone(), queue, cx).pip());
        player.update(cx, |player, cx| {
            player.set_shown(true, cx);
            player.focus(window, cx);
        });

        let theme = cx.theme();
        Self {
            background: ui::backdrop(theme.blur_window, theme.transparent),
            playback,
            title_bar: cx.new(TitleBar::new),
            field,
            player,
            toasts: cx.new(ToastStack::new),
        }
    }

    /// The bar's options, which are the fullscreen player's own.
    fn options(&self, cx: &App) -> TitleBarOptions {
        self.player.read(cx).title_bar(None, cx)
    }
}

impl Render for PipView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ambient = ambient::shown(cx);
        let cover = self
            .playback
            .read(cx)
            .shown_track(cx)
            .and_then(|track| track.cover)
            .filter(|_| ambient);
        self.field.update(cx, |field, cx| field.follow(cover, cx));
        let theme = match ambient {
            true => self.field.read(cx).theme(cx),
            false => *cx.theme(),
        };

        let options = self.options(cx);
        self.title_bar
            .update(cx, |bar, cx| bar.set_options(options, cx));
        window.set_rem_size(theme.font_size);
        let background = ui::backdrop(theme.blur_window, theme.transparent);
        if self.background != background {
            self.background = background;
            window.set_background_appearance(background);
        }

        let settings = Sonora::global(cx).settings.read(cx);
        let font = ui_font(settings.font());
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        let radius = crate::chrome::window_radius(settings, cx, window);
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        window.set_window_corner_radius(radius.unwrap_or_default());
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        let radius: Option<Pixels> = None;

        let alone = !cx
            .windows()
            .iter()
            .any(|window| window.downcast::<Root>().is_some());

        let root = div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .font(font)
            .text_color(theme.foreground)
            .when_some(radius, |this, radius: Pixels| {
                this.rounded(radius).overflow_hidden()
            })
            .map(|this| match ambient {
                true => this.child(self.field.clone()),
                false => this.bg(theme.background),
            })
            .map(middle_scrolled)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &MinimizeWindow, window, _| window.minimize_window())
            .child(self.title_bar.clone())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(self.player.clone()),
            )
            .when(alone, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(FullscreenView::dock_height(cx))
                        .child(deferred(self.toasts.clone())),
                )
            })
            .child(ui::lingers());
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        let root = root.child(WindowFrame::new());
        Themed::new(theme, root)
    }
}
