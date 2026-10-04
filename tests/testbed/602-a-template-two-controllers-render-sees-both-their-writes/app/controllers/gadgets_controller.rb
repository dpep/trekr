class GadgetsController
  def show
    @widget = Gadget.new
    render template: "widgets/show"
  end

  def index
    @title = Gadget.new
  end
end
