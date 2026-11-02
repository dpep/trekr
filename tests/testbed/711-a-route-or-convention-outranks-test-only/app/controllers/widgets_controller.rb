class WidgetsController
  include Pundit::Authorization

  def index
  end

  def publish
    authorize Widget.new
  end
end
