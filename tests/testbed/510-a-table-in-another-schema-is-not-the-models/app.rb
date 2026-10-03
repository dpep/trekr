class Widget < ActiveRecord::Base
end

module Audit
  class Widget < ActiveRecord::Base
    self.table_name = "audit.widgets"
  end
end

class RecentWidget < ActiveRecord::Base
end

class Job
  def run
    Widget.new.name.upcase
    Widget.new.action
    Audit::Widget.new.action.upcase
    RecentWidget.new.name.upcase
  end
end
