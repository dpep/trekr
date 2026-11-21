class Widget
  def ready?; false; end
end
class Shop
  def self.build
    w = Widget.new
    def w.ready?; true; end
    w
  end
end
