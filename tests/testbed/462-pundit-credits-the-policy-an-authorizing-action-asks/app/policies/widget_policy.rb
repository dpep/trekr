class WidgetPolicy < ApplicationPolicy
  def index?
  end

  def archive?
  end

  def ping?
  end
end

class GadgetPolicy < ApplicationPolicy
  def archive?
  end
end
