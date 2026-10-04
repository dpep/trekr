class Widget
  class << self
    def build
      new
    end

    private

    def register(name)
      name
    end
  end

  def self.open
    :open
  end
  private_class_method :open

  private_class_method def self.secret
    :secret
  end
end

class Gadget < Widget
  register :gear
end
