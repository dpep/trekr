class Widget
  def label
    "w"
  end
end

class Gadget
  def label
    "g"
  end
end

class Shop
  def self.build
    w = Widget.new
    def w.label
      "special"
    end
    w.label
  end
end
Shop.build
Gadget.new.label
