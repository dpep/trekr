class WidgetsController
  def index
    @widget = Widget.new
    @widgets = Widget.where(shown: true)
  end
end
