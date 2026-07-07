/// Optional LS_COLORS support.

/// Get an `lscolors::LsColors` instance from an optional env var value.
pub(crate) fn get_ls_colors(env: Option<&str>) -> Option<lscolors::LsColors> {
    Some(env.map_or_else(
        lscolors::LsColors::default,
        lscolors::LsColors::from_string,
    ))
}
