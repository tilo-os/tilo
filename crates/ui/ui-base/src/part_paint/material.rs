//! What a floating surface's body is made of, and painting it. A surface names its material; the
//! toolkit and compositor decide how it renders (`docs/design-language.md`, "Material and
//! transparency").

use vello::kurbo::Affine;

/// What a floating surface's body is made of. A surface names its material;
/// the toolkit and compositor decide how it renders — the blur radius, the
/// tint and the opaque fallback are never the caller's
/// (`docs/design-language.md`, "Material and transparency").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Material {
    /// Floating chrome — palette, menus, popovers, quick settings, sheets,
    /// notifications, and chrome over media: the blurred backdrop tinted
    /// with `surfaces.overlay`. Opaque `surfaces.overlay` where the renderer
    /// cannot blur, and under reduced transparency or forced colours.
    #[default]
    Floating,
    /// Always opaque `surfaces.overlay`: a surface whose backdrop must not
    /// show through, such as a prompt for a secret over content the user
    /// does not own.
    Opaque,
    /// `surfaces.overlay` at `surfaces.see_through_opacity` (half), over an
    /// unblurred backdrop: chrome that must leave the exact pixels under it
    /// visible, such as the capture bar over what is being chosen. Opaque
    /// under reduced transparency, forced colours and high contrast.
    SeeThrough,
}

/// Paint a floating surface's body: `shape` filled with `material`. Edges,
/// shadows and content are drawn by the caller on top, exactly as on an
/// opaque surface.
pub fn paint_material(
    scene: &mut vello::Scene,
    transform: Affine,
    shape: &impl vello::kurbo::Shape,
    material: Material,
    theme: &crate::theme::Theme,
) {
    super::backdrop::fill(scene, transform, shape, material, theme);
}
