pub mod ui;
pub mod gui;


pub const ICON_GREEN: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 22 22">
  <circle cx="6"  cy="11" r="3.5" fill="none" stroke="#1D9E75" stroke-width="2"/>
  <circle cx="6"  cy="11" r="1.5" fill="#1D9E75"/>
  <circle cx="16" cy="11" r="3.5" fill="none" stroke="#1D9E75" stroke-width="2"/>
  <circle cx="16" cy="11" r="1.5" fill="#1D9E75"/>
  <line x1="9.5" y1="11" x2="12.5" y2="11" stroke="#1D9E75" stroke-width="2" stroke-linecap="round"/>
  <circle cx="18" cy="4" r="4" fill="#1D9E75"/>
  <path d="M15.5 4 L17.5 6 L20.5 2" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
</svg>"##;

pub const ICON_AMBER: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 22 22">
  <circle cx="6"  cy="11" r="3.5" fill="none" stroke="#EF9F27" stroke-width="2"/>
  <circle cx="6"  cy="11" r="1.5" fill="#EF9F27"/>
  <circle cx="16" cy="11" r="3.5" fill="none" stroke="#EF9F27" stroke-width="2" opacity="0.4"/>
  <circle cx="16" cy="11" r="1.5" fill="#EF9F27" opacity="0.4"/>
  <line x1="9.5" y1="11" x2="12.5" y2="11" stroke="#EF9F27" stroke-width="2" stroke-linecap="round" stroke-dasharray="2 2"/>
  <circle cx="18" cy="4" r="4" fill="#EF9F27"/>
  <text x="18" y="5" font-size="6" font-weight="bold" text-anchor="middle" dominant-baseline="central" fill="#fff">!</text>
</svg>"##;

pub const ICON_RED: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 22 22">
  <circle cx="6"  cy="11" r="3.5" fill="none" stroke="#E24B4A" stroke-width="2" opacity="0.4"/>
  <circle cx="6"  cy="11" r="1.5" fill="#E24B4A" opacity="0.4"/>
  <circle cx="16" cy="11" r="3.5" fill="none" stroke="#E24B4A" stroke-width="2" opacity="0.4"/>
  <circle cx="16" cy="11" r="1.5" fill="#E24B4A" opacity="0.4"/>
  <line x1="9.5" y1="11" x2="12.5" y2="11" stroke="#E24B4A" stroke-width="2" stroke-linecap="round" opacity="0.25"/>
  <line x1="9.5" y1="8.5" x2="12.5" y2="13.5" stroke="#E24B4A" stroke-width="2" stroke-linecap="round"/>
  <line x1="12.5" y1="8.5" x2="9.5" y2="13.5" stroke="#E24B4A" stroke-width="2" stroke-linecap="round"/>
  <circle cx="18" cy="4" r="4" fill="#E24B4A"/>
  <line x1="16" y1="4" x2="20" y2="4" stroke="#fff" stroke-width="2" stroke-linecap="round"/>
</svg>"##;
