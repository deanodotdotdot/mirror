"""Two palettes for extension lifecycle tests."""

from mirror.cli import extension, themes


class ThemeExtension(extension.Extension[None, None, None]):
  @classmethod
  def id(cls) -> str:
    return 'test-themes'

  def themes(self) -> list[themes.Theme]:
    return [
      themes.Theme('night', 'Night sky', {'accent': '#123456'}),
      themes.Theme('day', 'Day sky', {'accent': '#654321'}, base='light'),
    ]

  async def on_background(self, context: extension.Context) -> None:
    raise ValueError('palette provider failed')
