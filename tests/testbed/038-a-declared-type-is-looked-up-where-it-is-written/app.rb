module Shop
  class Item
    def weigh
    end
  end

  class Order
    sig { returns(Item) }
    def item
    end
  end
end

class Item
  def weigh
  end
end

class Shop::Flat
  sig { returns(Item) }
  def item
  end
end

class User
  def greet
  end
end

module Admin
  class User
    def greet
    end
  end

  class Post
    belongs_to :user

    def go
      user.greet
    end
  end
end

class Admin::Note
  belongs_to :user

  def go
    user.greet
  end
end

class Job
  def run
    Shop::Order.new.item.weigh
    Shop::Flat.new.item.weigh
  end
end
