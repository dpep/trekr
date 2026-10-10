module App
  OUTER = 1
  class Widget
    OWN = 2
    class << self
      LIMIT = 3
      STALE = 4
      def limit = LIMIT
      def own = OWN
      def outer = OUTER
      module Helper
        def self.build = LIMIT
      end
      def helper = Helper
    end
    def size = LIMIT
    def stale = STALE
  end
  Widget::LIMIT
  Widget::STALE
end
App::Widget.limit
App::Widget.helper
class Base
  BASE = 5
end
class Sub < Base
  class << self
    def base = BASE
  end
  def base = BASE
end
class Gadget
  def self.make(item)
    class << item
      TAG = 6
      def tag = TAG
    end
  end
  def tag = TAG
end
