module Pricing
  def price
  end
end

module Stocking
  def stock
  end
end

module Counting
  def count_all
  end
end

class Shelf
  ActiveSupport.run_load_hooks(:shelf, self)
end

ActiveSupport.on_load(:shelf) { |base| base.include(Pricing) }
ActiveSupport.on_load(:shelf, yield: true) { |shelf| shelf.include(Stocking) }
ActiveSupport.on_load(:shelf) { |base| base.extend(Counting) if base.frozen? }

Shelf.new.price
Shelf.new.stock
Shelf.count_all

class Shelf
  def label
  end

  def setup
  end
end

ActiveSupport.on_load(:shelf) do
  def label
    super
  end

  def tidy
  end
end

class Railtie
  initializer do
    ActiveSupport.on_load(:shelf) do
      def setup
      end
    end
  end
end

Shelf.new.label
Shelf.new.tidy
Shelf.new.setup
