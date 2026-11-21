require "gizmo/extra"

class Report
  def client
    WidgetKit::Client.new
  end

  def page
    paginate_widgets
  end

  def core
    SuiteCore.version
  end
end
