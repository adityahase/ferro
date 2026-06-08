"""Shared permissive stub used by the shim's module-level __getattr__ fallbacks AND by
ferro_boot's lazy import finder.

`Stub` is a *type* (so it can be subclassed, called, decorated-with, subscripted, or used in a
`X | None` annotation), and instances absorb arithmetic / concatenation / comparison / f-string
formatting — whatever an absent dependency happens to be used as while a controller's module body
executes at import time. Goal: every real controller IMPORTS, so the memory ceiling is honest.
"""

# operators an absent value might be put through at module-execution time
_BINOPS = ["add", "radd", "sub", "rsub", "mul", "rmul", "truediv", "rtruediv",
           "floordiv", "rfloordiv", "mod", "rmod", "pow", "rpow",
           "or", "ror", "and", "rand", "xor", "rxor", "lshift", "rlshift",
           "rshift", "rrshift", "matmul"]


class _Inst:
    def __init__(self, *a, **k): pass
    def __getattr__(self, k): return Stub
    def __call__(self, *a, **k):
        if len(a) == 1 and not k and callable(a[0]):
            return a[0]
        return Stub
    def __getitem__(self, k): return Stub
    def __setitem__(self, k, v): pass
    def __delitem__(self, k): pass
    def __iter__(self): return iter(())
    def __next__(self): raise StopIteration
    def __enter__(self): return self
    def __exit__(self, *a): return False
    def __contains__(self, x): return False
    def __len__(self): return 0
    def __bool__(self): return False
    def __int__(self): return 0
    def __float__(self): return 0.0
    def __index__(self): return 0
    def __str__(self): return ""
    def __repr__(self): return "<stub>"
    def __format__(self, spec): return ""
    def __hash__(self): return 0
    def __neg__(self): return self
    def __pos__(self): return self
    def __abs__(self): return self
    def __lt__(self, o): return False
    def __le__(self, o): return False
    def __gt__(self, o): return False
    def __ge__(self, o): return False


def _make_binop(name):
    def op(self, other):
        return _Inst()
    op.__name__ = f"__{name}__"
    return op


for _n in _BINOPS:
    setattr(_Inst, f"__{_n}__", _make_binop(_n))


class _Meta(type):
    def __getattr__(cls, k): return Stub
    def __call__(cls, *a, **k):
        if len(a) == 1 and not k and callable(a[0]):
            return a[0]
        return _Inst()
    def __getitem__(cls, k): return Stub
    def __iter__(cls): return iter(())
    def __len__(cls): return 0


for _n in _BINOPS:
    setattr(_Meta, f"__{_n}__", _make_binop(_n))


class Stub(metaclass=_Meta):
    pass


def stub_attr(name):
    """Module-level __getattr__ fallback: resolve unknown names to the permissive Stub type."""
    if name.startswith("__") and name.endswith("__"):
        raise AttributeError(name)
    return Stub
