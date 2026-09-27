require "delegate"

class Wrapped < DelegateClass(Base)
  def greet
    "wrapped-" + super
  end
end
