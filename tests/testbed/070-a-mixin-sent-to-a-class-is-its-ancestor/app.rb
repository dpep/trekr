class Engine
  def boot
  end
end

module BootHook
  def boot
    super
  end
end

Engine.prepend(BootHook)

module Optional
end

Engine.prepend(Optional) if defined?(Optional::Ready)

module Setup
  def self.install
    Engine.prepend(Optional)
  end
end

module Shop
  class Cart
  end

  module Pricing
    def price
    end
  end

  module Finder
    def locate
    end
  end

  Cart.include(Pricing)
  Cart.send(:extend, Finder)
  Missing.include(Pricing)
end

Shop::Cart.new.price
Shop::Cart.locate

module Audit
  def audit
  end

  Engine.include(self)
end

Engine.new.audit
