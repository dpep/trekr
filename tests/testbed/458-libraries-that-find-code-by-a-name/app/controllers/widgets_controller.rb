class WidgetsController
  include Pundit::Authorization

  def publish
    authorize Widget.new
  end
end
