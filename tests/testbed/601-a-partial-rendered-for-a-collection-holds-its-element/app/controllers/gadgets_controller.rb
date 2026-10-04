class GadgetsController
  def index
    @gadgets = Gadget.where(shown: true)
  end
end
