module Lookup
  def find(id)
  end

  def id_for(key)
  end
end

module Tagging
  def tag
  end
end

module Hidden
  def hidden
  end
end

ActiveSupport.on_load(:engine) do
  extend Lookup
end

ActiveSupport.on_load(:controller) { include Tagging }
ActiveSupport.on_load(:controller, yield: true) { |c| include Hidden }

class Widget < Engine::Base
end

Widget.find(1)
Widget.id_for(:a)
Controller.new.tag
Controller.new.hidden

module Late
  def late
  end
end

class Railtie
  initializer do
    ActiveSupport.on_load(:controller) { include Late }
  end
end

Controller.new.late
Railtie.new.late
