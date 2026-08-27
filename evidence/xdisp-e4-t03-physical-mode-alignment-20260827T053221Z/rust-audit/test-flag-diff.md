# `GUD_TEST_DYNAMIC_MODE_MATCH` audit

Before E4-T03 the flag gated maximum-shadow preallocation, dynamic route-state
initialization, SET_STATE CHECK plan creation, SET_STATE COMMIT route execution,
and pending-candidate invalidation on protocol lifecycle events. It did not
change connector selection, physical mode enumeration, GUD mode list/order,
preferred mode, format validation, connector ownership, or USB receive logic.

After E4-T03 exact routing is enabled by `physical_mode_routing_enabled()` in
normal runtime. The parser still accepts exactly `GUD_TEST_DYNAMIC_MODE_MATCH=1`
so old drop-ins do not break startup, but the value is an obsolete compatibility
no-op and emits a warning. The physical startup override remains independently
available for diagnostics and does not change advertisement order.
