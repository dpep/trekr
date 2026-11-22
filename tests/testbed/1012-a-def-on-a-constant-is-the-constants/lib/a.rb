class Widget
end

CONF = Widget.new

def CONF.alpha
  :a
end

module Settings
  STORE = Object.new

  def STORE.gamma
    :g
  end
end

module App
  def self.run
    CONF.alpha
    Settings::STORE.gamma
  end
end
App.run
