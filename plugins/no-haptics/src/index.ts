// @description Disables haptic feedback and vibration across the app
// @version 0.1.0
// @author cocomeshy
// @enabled false

if (phoenix.ui.disableHaptics) {
  phoenix.ui.disableHaptics();
} else {
  phoenix.log.warn("disableHaptics is not supported by this loader version");
}
