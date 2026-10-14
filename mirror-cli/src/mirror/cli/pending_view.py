"""Creation state presented through the ordinary workspace composer."""

from collections.abc import Callable
from typing import override

from mirror.protocol import agent_pb2
from prompt_toolkit import buffer
from prompt_toolkit.application import current
from prompt_toolkit.layout import containers, controls, layout

from mirror.cli import extension, selection, state, themes, workspaces
from mirror.cli.components import component
from mirror.cli.composer import pane, surface
from mirror.cli.sandbox import view as sandbox_view


def layout_for(
  workspace: workspaces.PendingWorkspace,
  get_menu: Callable[[], state.MenuState],
  navigation: containers.AnyContainer,
  draft: buffer.Buffer,
  theme_name: Callable[[], str],
  extensions: extension.ExtensionView,
) -> layout.Layout:
  """Supply creation state, not a separate composer or widget layout."""
  sandbox = (
    workspace if isinstance(workspace, workspaces.SandboxWorkspace) else None
  )

  def view_state() -> state.State:
    choices = sandbox_view.choices(sandbox) if sandbox is not None else ()
    label = 'worktree' if workspace.label == 'creating…' else workspace.label
    preferences = workspace.preferences
    source = sandbox.source.agent if sandbox is not None else None
    return state.State(
      session=state.SessionState(conversation_id=''),
      lifecycle=state.LifecycleState(active=True, failure=workspace.error),
      selector=state.SelectorState(
        menu=get_menu(),
        selector=selection.Selector(
          'sandbox', 'Sandbox', choices, str, numbered=True
        )
        if choices
        else None,
      ),
      composer=workspace.draft,
      layout=state.LayoutState(
        columns=current.get_app().output.get_size().columns
      ),
      extensions=extensions.initial_states.configured(
        themes.EXTENSION_ID, {'name': theme_name()}
      ),
      activity=state.ActivityState(
        busy=workspace.error is None and not choices,
        status=f'{sandbox.phase} · Esc to cancel'
        if sandbox is not None
        else workspace.notice or f'Creating {label}…',
      ),
      status_bar=state.StatusBarState(
        source.model if source is not None else preferences.model or '…',
        source.reasoning_effort
        if source is not None
        else preferences.reasoning_effort or 'none',
        source.approval_mode
        if source is not None
        else (
          'auto'
          if preferences.approval_mode == agent_pb2.APPROVAL_MODE_AUTO
          else 'approve'
        ),
      ),
    )

  prompt = surface.input_window(
    draft,
    hidden=lambda: view_state().selector.numbered,
    max_height=lambda: max(1, current.get_app().output.get_size().rows // 3),
  )
  widgets = pane.components(extensions, lambda: composer.selector_max_height())
  widgets['failure'] = _CreationNotice(workspace)
  composer = pane.Pane(
    view_state,
    component.ComponentHost(view_state),
    component.ComponentCollection(widgets),
    prompt,
    navigation,
    fullscreen=True,
  )
  return layout.Layout(
    containers.HSplit(
      [containers.Window(), composer.dock], style='class:theme-root'
    ),
    focused_element=prompt,
  )


class _CreationNotice(component.Component):
  def __init__(self, workspace: workspaces.PendingWorkspace) -> None:
    self._workspace = workspace

  @override
  def render(self, state: state.State) -> containers.AnyContainer:
    del state
    workspace = self._workspace
    message = workspace.error or ''
    if workspace.error and not isinstance(
      workspace, workspaces.SandboxWorkspace
    ):
      message += ' · Enter to dismiss'
    return containers.Window(
      controls.FormattedTextControl([('class:worktree-failed', message)]),
      height=int(bool(message)),
    )
