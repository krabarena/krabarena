from __future__ import annotations

import re
from typing import Any

from playwright.sync_api import Locator, Page, Response, expect

from helpers.navigation import open_target_page
from helpers.results import TestArtifacts


class ActionBot:
    """Small user-action helper for shared logging and pre-action checks."""

    def __init__(self, artifacts: TestArtifacts) -> None:
        self.artifacts = artifacts

    def open_page(
        self,
        page: Page,
        path: str,
        *,
        base_url: str,
        description: str,
    ) -> Response | None:
        self.record_step("open_page", description=description, path=path)
        response = open_target_page(page, path, base_url=base_url)
        self.artifacts.set_current_url(page.url)
        return response

    def click(self, locator: Locator, *, description: str) -> None:
        self.record_step("click", description=description)
        expect(locator).to_be_visible()
        expect(locator).to_be_enabled()
        locator.click()

    def fill(self, locator: Locator, value: str, *, description: str) -> None:
        self.record_step("fill", description=description, value=value)
        expect(locator).to_be_visible()
        expect(locator).to_be_enabled()
        locator.fill(value)

    def record_step(self, name: str, **data: Any) -> None:
        stable_name = _stable_step_name(name, data)
        steps = self.artifacts.runtime_data.setdefault("steps", [])
        if not isinstance(steps, list):
            steps = []
            self.artifacts.runtime_data["steps"] = steps
        self.artifacts.runtime_data["current_step"] = stable_name
        steps.append({"name": stable_name, **data})


def _stable_step_name(name: str, data: dict[str, Any]) -> str:
    description = str(data.get("description") or "")
    lower_description = description.lower()
    path = str(data.get("path") or "")

    if name == "open_page":
        if path == "/":
            return "open-home"
        if path.startswith("/battles/"):
            return "open-battle-detail"
        if path.startswith("/claims/"):
            return "open-claim-detail"
        if path.startswith("/search"):
            return "open-search"
        if path.startswith("/login"):
            return "open-login"

    if name in {"submit_topnav_search", "submit_search"}:
        return "submit-search"

    if "submit" in lower_description and "search" in lower_description:
        return "submit-search"
    if "search result" in lower_description:
        return "open-search-result"
    if "login page" in lower_description:
        return "open-login"
    if "google oauth" in lower_description:
        return "start-google-oauth"
    if "github oauth" in lower_description:
        return "start-github-oauth"
    if "first claim" in lower_description or "claim detail" in lower_description:
        return "open-claim-detail"
    if "first battle" in lower_description or "battle candidate" in lower_description:
        return "open-battle-detail"

    return _kebab_case(name)


def _kebab_case(value: str) -> str:
    stripped = value.strip().replace("_", "-")
    replaced = re.sub(r"[^a-zA-Z0-9-]+", "-", stripped)
    collapsed = re.sub(r"-+", "-", replaced)
    return collapsed.strip("-").lower() or "unknown-step"
