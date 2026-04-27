"""bock-lang — placeholder for the Bock programming language.

This package reserves the ``bock-lang`` namespace on PyPI for
future Python tooling around the Bock language. Bock itself is
implemented in Rust and transpiles to multiple targets; Python
tooling (bindings, test runners, etc.) is post-v1.0 work.

For the actual language and compiler, visit:
    https://bocklang.org

To install the Bock compiler:
    cargo install bock

This package contains no functional code. Importing it prints
a notice and exits. If you've installed it expecting real
functionality, you're early — follow https://github.com/bock-lang/bock
for updates.
"""

__version__ = "0.0.1"
__all__: list[str] = []

_NOTICE = """\
The 'bock-lang' Python package is currently a namespace placeholder.

Bock is a feature-declarative, target-agnostic programming language
implemented in Rust. The compiler is available via:

    cargo install bock

Python tooling for Bock is planned for post-v1.0 releases. Follow
progress at https://bocklang.org and https://github.com/bock-lang/bock.
"""


def _show_notice() -> None:
    """Print the placeholder notice. Called automatically on import."""
    import sys
    print(_NOTICE, file=sys.stderr)


_show_notice()
