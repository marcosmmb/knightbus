const GAP_PX = 8;
const EDGE_PX = 12;
const DISMISS_DELAY_MS = 150;
const tooltip = document.getElementById('tooltip');
let active = null;
let timer = null;
function hide() {
  if (active) active.removeAttribute('aria-describedby');
  active = null; tooltip.hidden = true;
}
function show(target) {
  clearTimeout(timer); hide(); active = target;
  // A tooltip must live inside an open modal to appear above its backdrop.
  (target.closest('dialog') || document.querySelector('main')).append(tooltip);
  tooltip.textContent = target.dataset.tooltip; tooltip.hidden = false;
  target.setAttribute('aria-describedby', 'tooltip');
  const rect = target.getBoundingClientRect();
  const width = tooltip.offsetWidth, height = tooltip.offsetHeight;
  tooltip.style.left = `${Math.max(EDGE_PX, Math.min(rect.left, innerWidth - width - EDGE_PX))}px`;
  const below = rect.bottom + GAP_PX;
  tooltip.style.top = `${below + height < innerHeight - EDGE_PX ? below : Math.max(EDGE_PX, rect.top - height - GAP_PX)}px`;
}
function dismiss() { timer = setTimeout(hide, DISMISS_DELAY_MS); }
document.addEventListener('pointerover', event => {
  if (tooltip.contains(event.target)) { clearTimeout(timer); return; }
  const target = event.target.closest('[data-tooltip]');
  if (target && target !== active) show(target);
});
document.addEventListener('pointerout', event => {
  if (event.target.closest('[data-tooltip]') || tooltip.contains(event.target)) dismiss();
});
document.addEventListener('focusin', event => {
  const target = event.target.closest('[data-tooltip]');
  if (target) show(target); else hide();
});
document.addEventListener('focusout', dismiss);
document.addEventListener('keydown', event => { if (event.key === 'Escape') hide(); });
document.addEventListener('scroll', hide, true);
window.addEventListener('resize', hide);
document.addEventListener('close', hide, true);
