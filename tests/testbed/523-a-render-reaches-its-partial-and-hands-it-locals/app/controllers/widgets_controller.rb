class WidgetsController
  def show
    @widget = Widget.new
    @widgets = Widget.where(shown: true).order(:name)
  end
end
