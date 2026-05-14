from __future__ import annotations

import sys

from .overlay import run_overlay_helper
from .ui import AgentDictateGtkApp


def main(argv: list[str] | None = None) -> int:
    argv = argv or sys.argv
    if "--overlay-helper" in argv[1:]:
        return run_overlay_helper()
    app = AgentDictateGtkApp()
    return app.run(argv)


if __name__ == "__main__":
    raise SystemExit(main())
