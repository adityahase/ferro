"""frappe.boot — holds the merged app hooks (populated by ferro_boot at startup)."""

# merged across installed apps: {hook_name: value} where value is list or dict-of-lists
_MERGED_HOOKS = {}


def set_hooks(merged):
    global _MERGED_HOOKS
    _MERGED_HOOKS = merged or {}


def get_hooks(hook=None, default=None, app_name=None):
    if hook is None:
        return _MERGED_HOOKS
    val = _MERGED_HOOKS.get(hook)
    if val is None:
        return default if default is not None else []
    return val
