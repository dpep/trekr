class Widget
  def spin; end
end

class Gadget
  def spin; end
end

class Builder
  extend T::Sig

  sig { returns(Widget) }
  def self.build; end

  def again
    self.class.build.spin
  end
end

class Kit
  extend T::Sig

  sig { returns(Gadget) }
  def build; end
end

def assemble(factory, model)
  factory.build.spin
  model.class.build.spin
end
