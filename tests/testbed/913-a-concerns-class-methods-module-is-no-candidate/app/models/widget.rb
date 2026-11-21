class Widget
  include Sluggable
  include Taggable

  module Helpers
  end
end
Widget.find_by_slug("x")
Widget.tagged("y")
