from __future__ import annotations

import ctypes
import ctypes.util

from .gtk import Gtk


class CtypesAppIndicator:
    CATEGORY_APPLICATION_STATUS = 0
    STATUS_ACTIVE = 1

    def __init__(self, indicator_id: str, icon_name: str) -> None:
        library_path = (
            ctypes.util.find_library("ayatana-appindicator3")
            or ctypes.util.find_library("appindicator3")
            or "libappindicator3.so.1"
        )
        self._lib = ctypes.CDLL(library_path)
        self._lib.app_indicator_new.argtypes = [
            ctypes.c_char_p,
            ctypes.c_char_p,
            ctypes.c_int,
        ]
        self._lib.app_indicator_new.restype = ctypes.c_void_p
        self._lib.app_indicator_set_status.argtypes = [ctypes.c_void_p, ctypes.c_int]
        self._lib.app_indicator_set_menu.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        if hasattr(self._lib, "app_indicator_set_icon_full"):
            self._lib.app_indicator_set_icon_full.argtypes = [
                ctypes.c_void_p,
                ctypes.c_char_p,
                ctypes.c_char_p,
            ]
        if hasattr(self._lib, "app_indicator_set_icon"):
            self._lib.app_indicator_set_icon.argtypes = [
                ctypes.c_void_p,
                ctypes.c_char_p,
            ]
        self._indicator = self._lib.app_indicator_new(
            indicator_id.encode("utf-8"),
            icon_name.encode("utf-8"),
            self.CATEGORY_APPLICATION_STATUS,
        )
        if not self._indicator:
            raise RuntimeError("Could not create AppIndicator")
        self._menu: Gtk.Menu | None = None

    def set_status_active(self) -> None:
        self._lib.app_indicator_set_status(self._indicator, self.STATUS_ACTIVE)

    def set_menu(self, menu: Gtk.Menu) -> None:
        self._menu = menu
        self._lib.app_indicator_set_menu(self._indicator, ctypes.c_void_p(hash(menu)))

    def set_icon_full(self, icon_name: str, description: str) -> None:
        if hasattr(self._lib, "app_indicator_set_icon_full"):
            self._lib.app_indicator_set_icon_full(
                self._indicator,
                icon_name.encode("utf-8"),
                description.encode("utf-8"),
            )
        else:
            self.set_icon(icon_name)

    def set_icon(self, icon_name: str) -> None:
        if hasattr(self._lib, "app_indicator_set_icon"):
            self._lib.app_indicator_set_icon(
                self._indicator,
                icon_name.encode("utf-8"),
            )
