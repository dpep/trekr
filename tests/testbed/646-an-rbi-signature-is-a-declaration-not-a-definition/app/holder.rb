class Holder
  delegate :whirl, to: :sprocket
  def sprocket = Sprocket.new
  def go = whirl
end
