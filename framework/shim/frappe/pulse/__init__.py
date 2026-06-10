# frappe.pulse — minimal shim package. Real Frappe's `pulse` is telemetry; app code only reaches
# in here for the version helper (frappe.pulse.utils.get_frappe_version), which several apps call
# at import time to version-gate code paths. Providing a real submodule keeps that import from
# falling through to the permissive lazy-finder stub (which returns a non-numeric _Meta and breaks
# int(version.split(".")[0])).
