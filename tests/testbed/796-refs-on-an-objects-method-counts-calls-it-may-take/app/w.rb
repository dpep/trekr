class Widget
  def run; 1; end
end
class Gadget
  def run; 1; end
end
class Shop
  def self.build
    w = Widget.new
    class << w
      def extra; 2; end
    end
    w.extra
    Gadget.new.extra
    w
  end

  def self.dress(item)
    def item.tag; 3; end
    item.tag
  end
end
Shop.build; Shop.dress(Widget.new)
