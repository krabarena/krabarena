from __future__ import annotations

from abc import ABC, abstractmethod
from typing import Self


class LoadableComponent(ABC):
    """Base class for pages/components that can load and verify readiness."""

    def load(self) -> Self:
        self._load()
        self.expect_loaded()
        return self

    @abstractmethod
    def _load(self) -> None:
        """Load the page or component."""

    @abstractmethod
    def expect_loaded(self) -> None:
        """Assert that the page or component is ready for interaction."""
