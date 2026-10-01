class Gadget
  validates :name, **Gated.options(presence: true)

  def feature_on?; true; end

  def lonely; end
end
