//! The icon set, embedded as SVG. GPUI rasterizes each one at the size it is
//! drawn and tints it with the element's text colour, so one outline serves
//! every state.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString, Styled as _, Svg, svg};

macro_rules! icon_svg {
    ($body:literal) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">"#,
            $body,
            "</svg>"
        )
    };
}

/// (name, svg). Names are what [`icon`] takes.
const ICONS: &[(&str, &str)] = &[
    // --- editor tools
    ("select", icon_svg!(r#"<path d="M5 3.5l13 7.2-5.6 1.7-1.9 5.6z"/><path d="M13.5 14.5l4.5 5"/>"#)),
    ("arrow", icon_svg!(r#"<path d="M5 19L19 5"/><path d="M10 5h9v9"/>"#)),
    ("line", icon_svg!(r#"<path d="M5 19L19 5"/>"#)),
    ("rect", icon_svg!(r#"<rect x="3.5" y="5.5" width="17" height="13" rx="2.5"/>"#)),
    ("oval", icon_svg!(r#"<ellipse cx="12" cy="12" rx="8.5" ry="6.5"/>"#)),
    (
        "pen",
        icon_svg!(
            r#"<path d="M4 20l1.2-4.6L16.6 4a2 2 0 0 1 2.8 0l.6.6a2 2 0 0 1 0 2.8L8.6 18.8z"/><path d="M14.5 6l3.5 3.5"/>"#
        ),
    ),
    ("text", icon_svg!(r#"<path d="M5 6.5V5h14v1.5"/><path d="M12 5v14"/><path d="M9.5 19h5"/>"#)),
    ("counter", icon_svg!(r#"<circle cx="12" cy="12" r="8.5"/><path d="M10.4 9.6L12.4 8v8"/>"#)),
    (
        "highlight",
        icon_svg!(
            r#"<path d="M9 14.5l-2 2V19h3l1.6-1.6"/><path d="M8.5 14L16 6.5a1.8 1.8 0 0 1 2.5 0l1 1a1.8 1.8 0 0 1 0 2.5L12 17.5z"/><path d="M3.5 21h17"/>"#
        ),
    ),
    (
        "blur",
        icon_svg!(
            r#"<path d="M12 3.5c3.2 3.6 5.5 6.5 5.5 9.6a5.5 5.5 0 0 1-11 0c0-3.1 2.300-6 5.5-9.600z"/><path d="M9.500 13.500a2.600 2.600 0 0 0 2.300 2.500"/>"#
        ),
    ),
    (
        "pixelate",
        icon_svg!(
            r#"<rect x="4" y="4" width="7" height="7" rx="1"/><rect x="13" y="13" width="7" height="7" rx="1"/><path d="M14.500 5.500h4v4"/><path d="M5.500 14.500v4h4"/>"#
        ),
    ),
    (
        "erase",
        icon_svg!(
            r#"<path d="M8.500 19.500l-4-4a1.800 1.800 0 0 1 0-2.500l8-8a1.800 1.800 0 0 1 2.500 0l4.500 4.500a1.800 1.800 0 0 1 0 2.500l-7.500 7.500z"/><path d="M9.500 9l6 6"/><path d="M8.500 19.500h11"/>"#
        ),
    ),
    (
        "spotlight",
        icon_svg!(
            r#"<circle cx="12" cy="12" r="4"/><path d="M12 3v2.500M12 18.500V21M3 12h2.500M18.500 12H21M5.600 5.600l1.800 1.800M16.600 16.600l1.800 1.800M5.600 18.400l1.800-1.800M16.600 7.400l1.800-1.800"/>"#
        ),
    ),
    (
        "magnify",
        icon_svg!(
            r#"<circle cx="10.500" cy="10.500" r="6.500"/><path d="M15.500 15.500L20.500 20.500"/><path d="M10.500 8v5M8 10.500h5"/>"#
        ),
    ),
    ("crop", icon_svg!(r#"<path d="M7 3v14h14"/><path d="M3 7h14v14"/>"#)),
    (
        "ruler",
        icon_svg!(
            r#"<path d="M3.500 16.500l13-13 4 4-13 13z"/><path d="M7 13l1.800 1.800M10 10l1.800 1.800M13 7l1.800 1.800"/>"#
        ),
    ),
    (
        "pipette",
        icon_svg!(
            r#"<path d="M13 7l4 4"/><path d="M11.500 8.500l-7 7V19h3.500l7-7"/><path d="M14.500 5.500l1.200-1.200a2.300 2.300 0 0 1 3.200 0l.8.800a2.300 2.300 0 0 1 0 3.200L18.500 9.500z"/>"#
        ),
    ),
    // --- editor actions
    ("undo", icon_svg!(r#"<path d="M8 5L4 9l4 4"/><path d="M4 9h10a5.500 5.500 0 0 1 0 11H9"/>"#)),
    ("redo", icon_svg!(r#"<path d="M16 5l4 4-4 4"/><path d="M20 9H10a5.500 5.500 0 0 0 0 11h5"/>"#)),
    (
        "copy",
        icon_svg!(
            r#"<rect x="8.500" y="8.500" width="11.500" height="11.500" rx="2.500"/><path d="M5.500 15.500H5A1.500 1.500 0 0 1 3.500 14V5A1.500 1.500 0 0 1 5 3.500h9A1.500 1.500 0 0 1 15.500 5v.500"/>"#
        ),
    ),
    (
        "save",
        icon_svg!(r#"<path d="M12 4v11"/><path d="M7.500 11l4.500 4.500 4.500-4.500"/><path d="M4.500 19.500h15"/>"#),
    ),
    ("pin", icon_svg!(r#"<path d="M9 4h6l-1 6 3.500 3.500h-11L10 10z"/><path d="M12 13.500V20"/>"#)),
    (
        "scan-text",
        icon_svg!(
            r#"<path d="M4 8V5.500A1.500 1.500 0 0 1 5.500 4H8M16 4h2.500A1.500 1.500 0 0 1 20 5.500V8M20 16v2.500a1.500 1.500 0 0 1-1.500 1.500H16M8 20H5.500A1.500 1.500 0 0 1 4 18.500V16"/><path d="M8 9.500h8M8 12.500h8M8 15.500h5"/>"#
        ),
    ),
    (
        "frame",
        icon_svg!(
            r#"<rect x="3.500" y="3.500" width="17" height="17" rx="3.500"/><rect x="7.500" y="8" width="9" height="8" rx="1.200"/>"#
        ),
    ),
    (
        "trash",
        icon_svg!(
            r#"<path d="M4.500 7h15"/><path d="M9.500 7V4.500h5V7"/><path d="M6.500 7l.9 12a1.500 1.500 0 0 0 1.500 1.400h6.200a1.500 1.500 0 0 0 1.500-1.400l.9-12"/>"#
        ),
    ),
    ("zoom-in", icon_svg!(r#"<path d="M12 5.500v13M5.500 12h13"/>"#)),
    ("zoom-out", icon_svg!(r#"<path d="M5.500 12h13"/>"#)),
    (
        "fit",
        icon_svg!(
            r#"<path d="M4 9V5.500A1.500 1.500 0 0 1 5.500 4H9M15 4h3.500A1.500 1.500 0 0 1 20 5.500V9M20 15v3.500a1.500 1.500 0 0 1-1.500 1.500H15M9 20H5.500A1.500 1.500 0 0 1 4 18.500V15"/>"#
        ),
    ),
    (
        "folder",
        icon_svg!(
            r#"<path d="M3.500 7A1.500 1.500 0 0 1 5 5.500h4.200l2 2.300H19A1.500 1.500 0 0 1 20.500 9.300V17.500A1.500 1.500 0 0 1 19 19H5a1.500 1.500 0 0 1-1.500-1.500z"/>"#
        ),
    ),
    (
        "fill",
        icon_svg!(
            r#"<rect x="4" y="4" width="16" height="16" rx="3"/><path d="M4 14l10-10M9 20L20 9M4 9l5-5M14.500 20L20 14.500"/>"#
        ),
    ),
    (
        "resize",
        icon_svg!(
            r#"<rect x="3.500" y="3.500" width="17" height="17" rx="3"/><path d="M13.500 7.500h3v3M10.500 16.500h-3v-3M16.500 7.500l-9 9"/>"#
        ),
    ),
    // --- capture modes
    (
        "area",
        icon_svg!(
            r#"<path d="M4 9V6a2 2 0 0 1 2-2h3M15 4h3a2 2 0 0 1 2 2v3M20 15v3a2 2 0 0 1-2 2h-3M9 20H6a2 2 0 0 1-2-2v-3"/><path d="M12 9.500v5M9.500 12h5"/>"#
        ),
    ),
    (
        "fullscreen",
        icon_svg!(
            r#"<rect x="3" y="4.500" width="18" height="12.500" rx="2"/><path d="M8.500 20.500h7M12 17v3.500"/>"#
        ),
    ),
    (
        "window",
        icon_svg!(
            r#"<rect x="3.500" y="4.500" width="17" height="15" rx="2.500"/><path d="M3.500 9h17"/><path d="M6.500 6.800h.01M9 6.800h.01"/>"#
        ),
    ),
    (
        "scroll",
        icon_svg!(
            r#"<rect x="6" y="3" width="12" height="18" rx="3"/><path d="M12 7.500v9"/><path d="M9.500 14l2.500 2.500 2.500-2.500"/>"#
        ),
    ),
    (
        "repeat",
        icon_svg!(
            r#"<path d="M4.500 11a7.500 7.500 0 0 1 13.200-4.800L20 8.500"/><path d="M20 4.500v4h-4"/><path d="M19.500 13a7.500 7.500 0 0 1-13.200 4.800L4 15.500"/><path d="M4 19.500v-4h4"/>"#
        ),
    ),
    (
        "image",
        icon_svg!(
            r#"<rect x="3.500" y="4.500" width="17" height="15" rx="2.500"/><circle cx="8.700" cy="9.500" r="1.500"/><path d="M20.500 15l-4.500-4.500L6 19.500"/>"#
        ),
    ),
    // --- panel
    (
        "sparkle",
        icon_svg!(
            r#"<path d="M12 3.500l1.900 5 5 1.900-5 1.900-1.900 5-1.900-5-5-1.900 5-1.900z"/><path d="M18.500 15.500l.8 2 2 .8-2 .8-.8 2-.8-2-2-.8 2-.8z"/>"#
        ),
    ),
    (
        "layers",
        icon_svg!(
            r#"<path d="M12 3.500l8.500 4.500-8.500 4.500L3.500 8z"/><path d="M3.500 12.500L12 17l8.500-4.500"/><path d="M3.500 16.500L12 21l8.500-4.500"/>"#
        ),
    ),
    (
        "mic",
        icon_svg!(
            r#"<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5.500 11.500a6.500 6.500 0 0 0 13 0"/><path d="M12 18v3"/>"#
        ),
    ),
    ("stop", icon_svg!(r#"<rect x="6" y="6" width="12" height="12" rx="2.500"/>"#)),
    (
        "eye",
        icon_svg!(
            r#"<path d="M2.500 12S6 5.500 12 5.500 21.500 12 21.500 12 18 18.500 12 18.500 2.500 12 2.500 12z"/><circle cx="12" cy="12" r="2.800"/>"#
        ),
    ),
    (
        "eye-off",
        icon_svg!(
            r#"<path d="M4 4l16 16"/><path d="M9.500 5.900A9 9 0 0 1 12 5.500c6 0 9.500 6.500 9.500 6.500a15 15 0 0 1-2.600 3.400M6.300 7.300A15 15 0 0 0 2.500 12S6 18.500 12 18.500a9 9 0 0 0 3.700-.8"/><path d="M10 10.100a2.800 2.800 0 0 0 3.900 3.900"/>"#
        ),
    ),
    ("bolt", icon_svg!(r#"<path d="M13 3L5 13.500h6L10.500 21l8-10.500h-6z"/>"#)),
    (
        "at",
        icon_svg!(
            r#"<circle cx="12" cy="12" r="3.500"/><path d="M15.500 8.500v4.700a2.300 2.300 0 0 0 4.500.700A8.500 8.500 0 1 0 16.500 19"/>"#
        ),
    ),
    (
        "notch",
        icon_svg!(
            r#"<rect x="3" y="5" width="18" height="14" rx="2.500"/><path d="M9 5v1.500a1.500 1.500 0 0 0 1.500 1.500h3A1.500 1.500 0 0 0 15 6.500V5"/>"#
        ),
    ),
    (
        "settings",
        icon_svg!(
            r#"<path d="M4 7h9M17 7h3M4 17h3M11 17h9"/><circle cx="15" cy="7" r="2.200"/><circle cx="9" cy="17" r="2.200"/>"#
        ),
    ),
    ("minimize", icon_svg!(r#"<path d="M6 12h12"/>"#)),
    ("maximize", icon_svg!(r#"<rect x="5" y="5" width="14" height="14" rx="2.500"/>"#)),
    (
        "rows",
        icon_svg!(
            r#"<rect x="4" y="4.500" width="16" height="6" rx="1.800"/><rect x="4" y="13.500" width="16" height="6" rx="1.800"/>"#
        ),
    ),
    ("chevron-down", icon_svg!(r#"<path d="M6.500 9.500l5.500 5.500 5.500-5.500"/>"#)),
    ("send", icon_svg!(r#"<path d="M12 19V5"/><path d="M6 11l6-6 6 6"/>"#)),
    ("close", icon_svg!(r#"<path d="M6.500 6.500l11 11M17.500 6.500l-11 11"/>"#)),
    (
        "help",
        icon_svg!(
            r#"<circle cx="12" cy="12" r="8.500"/><path d="M9.700 9.500a2.400 2.400 0 1 1 3.500 2.100c-.8.500-1.200 1-1.200 1.900"/><path d="M12 16.500h.01"/>"#
        ),
    ),
    ("plus", icon_svg!(r#"<path d="M12 5.500v13M5.500 12h13"/>"#)),
    ("check", icon_svg!(r#"<path d="M5 12.500l4.500 4.500L19 7.500"/>"#)),
    (
        "chat",
        icon_svg!(
            r#"<path d="M4 6.500A2.500 2.500 0 0 1 6.500 4h11A2.500 2.500 0 0 1 20 6.500v7a2.500 2.500 0 0 1-2.500 2.500H11l-4.500 4v-4A2.500 2.500 0 0 1 4 13.500z"/>"#
        ),
    ),
    (
        "list",
        icon_svg!(
            r#"<path d="M8.500 6.500h11M8.500 12h11M8.500 17.500h11"/><path d="M4.500 6.500h.01M4.500 12h.01M4.500 17.500h.01"/>"#
        ),
    ),
    ("bookmark", icon_svg!(r#"<path d="M6.500 4.500h11v15.500l-5.500-4-5.500 4z"/>"#)),
    ("up", icon_svg!(r#"<path d="M12 19V6"/><path d="M6.500 11.500L12 6l5.500 5.500"/>"#)),
    (
        "cursor-off",
        icon_svg!(r#"<path d="M5 3.500l13 7.200-5.600 1.700-1.900 5.600z"/><path d="M3.500 20.500l17-17"/>"#),
    ),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let name = path.strip_prefix("icons/").and_then(|p| p.strip_suffix(".svg")).unwrap_or(path);
        Ok(ICONS.iter().find(|(n, _)| *n == name).map(|(_, svg)| Cow::Borrowed(svg.as_bytes())))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ICONS.iter().map(|(n, _)| SharedString::from(format!("icons/{n}.svg"))).collect())
    }
}

/// An icon by name, 16 px unless resized. Give it a `text_color`: an SVG
/// does not inherit one, and without it nothing is drawn.
pub fn icon(name: &'static str) -> Svg {
    debug_assert!(ICONS.iter().any(|(n, _)| *n == name), "no icon named {name}");
    svg().path(SharedString::from(format!("icons/{name}.svg"))).flex_none().size(gpui::px(16.))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_valid_svg_with_a_unique_name() {
        let mut seen = std::collections::HashSet::new();
        for (name, svg) in ICONS {
            assert!(seen.insert(*name), "{name} is listed twice");
            assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"), "{name}");
            // Balanced quotes and tags: the cheap way to catch a typo in a
            // hand-written path.
            assert_eq!(svg.matches('"').count() % 2, 0, "{name} has an unclosed attribute");
            assert_eq!(svg.matches('<').count(), svg.matches('>').count(), "{name} has an unclosed tag");
        }
        assert!(Assets.load("icons/arrow.svg").unwrap().is_some());
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
        // And each one parses the way GPUI's renderer will read it.
        for (name, svg) in ICONS {
            usvg::Tree::from_str(svg, &usvg::Options::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}
